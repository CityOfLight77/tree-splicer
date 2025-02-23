#![allow(dead_code)]
use std::collections::{HashMap, HashSet};

use rand::{prelude::StdRng, Rng, SeedableRng};
use tree_sitter::{Language, Node, Tree};

use tree_sitter_edit::{Editor, NodeId};

use crate::node_types::NodeTypes;

#[derive(Debug, Default)]
pub struct Edits<'a>(HashMap<usize, &'a [u8]>);

impl<'a> Editor for Edits<'a> {
    fn has_edit(&self, _tree: &Tree, node: &Node) -> bool {
        self.0.get(&node.id()).is_some()
    }

    fn edit(&self, _source: &[u8], tree: &Tree, node: &Node) -> Vec<u8> {
        debug_assert!(self.has_edit(tree, node));
        Vec::from(*self.0.get(&node.id()).unwrap())
    }
}

#[derive(Debug)]
struct Branches<'a>(HashMap<&'static str, Vec<&'a [u8]>>);

impl<'a> Branches<'a> {
    fn new(trees: Vec<(&'a [u8], &'a Tree)>) -> Self {
        let mut branches = HashMap::with_capacity(trees.len()); // min
        for (text, tree) in trees {
            let mut nodes = vec![tree.root_node()];
            while !nodes.is_empty() {
                let mut children = Vec::with_capacity(nodes.len()); // guesstimate
                for node in nodes {
                    branches
                        .entry(node.kind())
                        .or_insert_with(|| HashSet::with_capacity(1))
                        .insert(&text[node.byte_range()]);
                    let mut i = 0;
                    while let Some(child) = node.child(i) {
                        children.push(child);
                        i += 1;
                    }
                }
                nodes = children;
            }
        }
        Branches(
            branches
                .into_iter()
                .map(|(k, s)| (k, s.iter().copied().collect()))
                .collect(),
        )
    }

    fn possible(&self) -> usize {
        let mut possible_mutations = 0;
        for s in self.0.values() {
            possible_mutations += s.len() - 1;
        }
        possible_mutations
    }
}

fn parse(language: Language, code: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(language)
        .expect("Failed to set tree-sitter parser language");
    parser.parse(code, None).expect("Failed to parse code")
}

#[derive(Debug)]
pub struct Config {
    pub chaos: u8,
    pub deletions: u8,
    pub language: Language,
    // pub intra_splices: usize,
    pub inter_splices: usize,
    pub node_types: NodeTypes,
    pub seed: u64,
    pub tests: usize,
}

struct Splicer<'a> {
    language: Language,
    branches: Branches<'a>,
    chaos: u8,
    deletions: u8,
    kinds: Vec<&'static str>,
    // intra_splices: usize,
    inter_splices: usize,
    node_types: NodeTypes,
    trees: Vec<(&'a [u8], &'a Tree)>,
    remaining: usize,
    rng: StdRng,
}

impl<'a> Splicer<'a> {
    fn pick_usize(&mut self, n: usize) -> usize {
        self.rng.gen_range(0..n)
    }

    fn pick_idx<T>(&mut self, v: &Vec<T>) -> usize {
        self.pick_usize(v.len())
    }

    fn all_nodes<'b>(&self, tree: &'b Tree) -> Vec<Node<'b>> {
        let mut all = Vec::with_capacity(16); // min
        let root = tree.root_node();
        let mut cursor = tree.walk();
        let mut nodes: HashSet<_> = root.children(&mut cursor).collect();
        while !nodes.is_empty() {
            let mut next = HashSet::new();
            for node in nodes {
                debug_assert!(!next.contains(&node));
                all.push(node);
                let mut child_cursor = tree.walk();
                for child in node.children(&mut child_cursor) {
                    debug_assert!(child.id() != node.id());
                    debug_assert!(!next.contains(&child));
                    next.insert(child);
                }
            }
            nodes = next;
        }
        all
    }

    fn pick_node<'b>(&mut self, tree: &'b Tree) -> Node<'b> {
        let nodes = self.all_nodes(tree);
        if nodes.is_empty() {
            return tree.root_node();
        }
        *nodes.get(self.pick_idx(&nodes)).unwrap()
    }

    fn delete_node(&mut self, _text: &[u8], tree: &Tree) -> (usize, Vec<u8>) {
        let chaotic = self.rng.gen_range(0..100) < self.chaos;
        if chaotic {
            return (self.pick_node(tree).id(), Vec::new());
        }
        let nodes = self.all_nodes(tree);
        if nodes.iter().all(|n| !self.node_types.optional_node(n)) {
            return (self.pick_node(tree).id(), Vec::new());
        }
        let mut node = nodes.get(self.pick_idx(&nodes)).unwrap();
        while !self.node_types.optional_node(node) {
            node = nodes.get(self.pick_idx(&nodes)).unwrap();
        }
        (node.id(), Vec::new())
    }

    fn splice_node(&mut self, text: &[u8], tree: &Tree) -> (usize, Vec<u8>) {
        let chaotic = self.rng.gen_range(0..100) < self.chaos;

        let mut node = tree.root_node();
        let mut candidates = Vec::new();
        // When modified trees are re-parsed, their nodes may have novel kinds
        // not in Branches (candidates.len() == 0). Also, avoid not mutating
        // (candidates.len() == 1).
        while candidates.len() <= 1 {
            node = self.pick_node(tree);
            candidates = if chaotic {
                let kind_idx = self.rng.gen_range(0..self.kinds.len());
                let kind = self.kinds.get(kind_idx).unwrap();
                self.branches.0.get(kind).unwrap().clone()
            } else {
                self.branches
                    .0
                    .get(node.kind())
                    .cloned()
                    .unwrap_or_default()
            };
        }

        let idx = self.rng.gen_range(0..candidates.len());
        let mut candidate = candidates.get(idx).unwrap();
        // Try to avoid not mutating
        let node_text = &text[node.byte_range()];
        while candidates.len() > 1 && candidate == &node_text {
            let idx = self.rng.gen_range(0..candidates.len());
            candidate = candidates.get(idx).unwrap();
        }
        // eprintln!(
        //     "Replacing '{}' with '{}'",
        //     std::str::from_utf8(&text[node.byte_range()]).unwrap(),
        //     std::str::from_utf8(candidate).unwrap(),
        // );
        (node.id(), Vec::from(*candidate))
    }

    fn splice_tree(&mut self, text0: &[u8], mut tree: Tree) -> Option<Vec<u8>> {
        let splices = self.rng.gen_range(0..self.inter_splices);
        let mut text = Vec::from(text0);
        for _ in 0..splices {
            let (id, bytes) = if self.rng.gen_range(0..100) < self.deletions {
                self.delete_node(text.as_slice(), &tree)
            } else {
                self.splice_node(text.as_slice(), &tree)
            };
            let id = NodeId { id };
            let bytes = bytes.to_vec();
            let mut result = Vec::with_capacity(text.len() / 4); // low guesstimate
            tree_sitter_edit::render(
                &mut result,
                &tree,
                text.as_slice(),
                &tree_sitter_edit::Replace { id, bytes },
            )
            .ok()?;
            text = result.clone();
            tree = parse(self.language, &String::from_utf8_lossy(text.as_slice()));
        }
        Some(text)
    }
}

impl<'a> Iterator for Splicer<'a> {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;

        let tree_idx: usize = self.pick_usize(self.trees.len());
        let (text, tree) = *self.trees.get(tree_idx).unwrap();
        self.splice_tree(text, tree.clone())
    }
}

#[allow(clippy::needless_lifetimes)]
pub fn splice<'a>(
    config: Config,
    files: &'a HashMap<String, (Vec<u8>, Tree)>,
) -> impl Iterator<Item = Vec<u8>> + 'a {
    let trees: Vec<_> = files
        .iter()
        .map(|(_, (txt, tree))| (txt.as_ref(), tree))
        .collect();
    let branches = Branches::new(
        files
            .iter()
            .map(|(_, (txt, tree))| (txt.as_ref(), tree))
            .collect(),
    );
    let possible = branches.possible();
    if possible < config.tests {
        eprintln!("[WARN] Only {possible} possible mutations");
    }
    let rng = rand::rngs::StdRng::seed_from_u64(config.seed);
    let kinds = branches.0.keys().copied().collect();
    Splicer {
        chaos: config.chaos,
        deletions: config.deletions,
        language: config.language,
        branches,
        kinds,
        // intra_splices: config.intra_splices,
        inter_splices: config.inter_splices,
        node_types: config.node_types,
        remaining: std::cmp::min(config.tests, possible),
        rng,
        trees,
    }
}
