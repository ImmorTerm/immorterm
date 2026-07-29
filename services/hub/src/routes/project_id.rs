//! Where a project's saved id lives on disk.
//!
//! ImmorTerm's own per-project state used to sit in `<project>/.claude/`,
//! which was fine when Claude Code was the only vendor and misleading the
//! moment it wasn't: a Codex-only user has no reason to grow a `.claude`
//! directory, and a `.codex` twin would be worse still — one project's
//! identity would fork depending on which agent happened to write it first.
//! So the canonical home is `<project>/.immorterm/`.
//!
//! MIGRATION SHAPE (per CLAUDE.md's ordering rule — read the new location
//! first, cut readers over, only then stop writing the old): this reads BOTH,
//! preferring the new one, and nothing here moves or deletes an existing file.
//! A project that already has `.claude/project-id` keeps resolving to exactly
//! the same id, so no plan, task or memory partition shifts underneath it.
//! Only newly-initialized projects get the new path.
//!
//! Returns the RAW trimmed contents. Sanitization is deliberately left to each
//! caller: `plans`/`spaces` sanitize for traversal safety, `tasks` does not,
//! and quietly changing that would relocate existing task files.

use std::path::Path;

/// Probed in order. First non-empty hit wins.
const PROJECT_ID_PATHS: &[&str] = &[
    // NOTE: deliberately NOT `.immorterm/project-id`. That filename is already
    // taken by the IDENTITY system, where it holds a UUID (see
    // registry.rs::read_or_create_project). This cascade resolves a SLUG — a
    // path component under ~/.immorterm/{tasks,plans}. Reading the UUID file
    // here silently repoints a project at an empty task/plan set, which is
    // exactly what happened when the two were conflated.
    ".immorterm/project-slug", // canonical, vendor-neutral
    ".claude/project-id",      // legacy — still authoritative where it exists
];

/// Read a project's saved id, or `None` when neither file exists or both are
/// empty. Never panics on unreadable paths.
pub fn read_project_id_file(project_dir: &str) -> Option<String> {
    for rel in PROJECT_ID_PATHS {
        if let Ok(s) = std::fs::read_to_string(Path::new(project_dir).join(rel)) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("immorterm-pid-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    #[test]
    fn legacy_claude_location_still_resolves() {
        // The case that must not regress: an existing project keeps its id.
        let d = tmp("legacy");
        write(&d, ".claude/project-id", "my-project\n");
        assert_eq!(
            read_project_id_file(d.to_str().unwrap()).as_deref(),
            Some("my-project")
        );
        let _ = fs::remove_dir_all(&d);
    }

    /// REGRESSION GUARD. `.immorterm/project-id` holds the identity UUID, not a
    /// slug. Reading it here repointed a real project from 23 tasks to 1.
    #[test]
    fn identity_uuid_file_is_never_mistaken_for_a_slug() {
        let d = tmp("uuidfile");
        write(&d, ".immorterm/project-id", "e407e41a-bc07-4902-a737-e9e89af4620b\n");
        write(&d, ".claude/project-id", "immorterm-org\n");
        assert_eq!(
            read_project_id_file(d.to_str().unwrap()).as_deref(),
            Some("immorterm-org"),
            "the identity UUID must not win over the legacy slug"
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn immorterm_location_wins_when_both_exist() {
        let d = tmp("both");
        write(&d, ".claude/project-id", "old-id\n");
        write(&d, ".immorterm/project-slug", "new-id\n");
        assert_eq!(
            read_project_id_file(d.to_str().unwrap()).as_deref(),
            Some("new-id")
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn empty_file_falls_through_to_the_next_candidate() {
        let d = tmp("empty");
        write(&d, ".immorterm/project-slug", "   \n");
        write(&d, ".claude/project-id", "fallback\n");
        assert_eq!(
            read_project_id_file(d.to_str().unwrap()).as_deref(),
            Some("fallback")
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn none_when_neither_exists() {
        let d = tmp("none");
        assert_eq!(read_project_id_file(d.to_str().unwrap()), None);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn contents_are_returned_raw_for_the_caller_to_sanitize() {
        // `tasks` relies on the unsanitized value; sanitizing here would
        // silently relocate every existing project's task files.
        let d = tmp("raw");
        write(&d, ".immorterm/project-slug", "Weird Name/../x\n");
        assert_eq!(
            read_project_id_file(d.to_str().unwrap()).as_deref(),
            Some("Weird Name/../x")
        );
        let _ = fs::remove_dir_all(&d);
    }
}
