use anyhow::Result;

fn main() -> Result<()> {
    tree_splicer::cli::main(tree_sitter_solidity::language(), tree_sitter_solidity::NODE_TYPES)
}
