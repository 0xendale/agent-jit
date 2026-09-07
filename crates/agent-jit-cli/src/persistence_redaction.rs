use agent_jit_domain::redaction::{PathAliases, Redactor};

pub(crate) fn redactor(worktree_root: &str) -> Redactor {
    Redactor::new().with_path_aliases(PathAliases::new(
        &std::env::var("HOME").unwrap_or_default(),
        worktree_root,
    ))
}
