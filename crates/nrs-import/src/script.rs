//! Placeholder for `<script>` execution (Cargo feature `scripting`).
use crate::xtree::XNode;

pub fn run_entry_script(_node: &XNode) -> Result<(), String> {
    Err("scripting not yet implemented".into())
}
