use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: String,       // Absolute path to SKILL.md
    pub directory: String,  // Directory containing SKILL.md
    pub is_workspace: bool, // True if located in current workspace
}

impl Skill {
    pub fn prompt_reference(&self) -> String {
        format!("${} ", self.name)
    }
}

pub fn requested_skills_for_prompt(input: &str, skills: &[Skill]) -> String {
    let input = input.to_lowercase();
    let requested = skills.iter().filter(|skill| {
        let token = format!("${}", skill.name.to_lowercase());
        let named = input.match_indices(&token).any(|(start, _)| {
            let word_char = |ch: char| ch.is_alphanumeric() || matches!(ch, '_' | '-' | '$');
            let before = input[..start].chars().next_back();
            let after = input[start + token.len()..].chars().next();
            !before.is_some_and(word_char) && !after.is_some_and(word_char)
        });
        let linked = input.contains(&format!("]({})", skill.path.to_lowercase()));
        named || linked
    }).collect::<Vec<_>>();
    if requested.is_empty() { return String::new(); }
    let mut section = "\n<requested_skills>\nThe user explicitly selected these skills for the current prompt. Read each SKILL.md using read_skill or read_file before working on the task, and follow its instructions within the user's request:\n".to_string();
    for skill in requested {
        section.push_str(&format!("- {}: {}\n", skill.name, skill.path));
    }
    section.push_str("</requested_skills>\n");
    section
}

pub fn completion_skills(workspace: &Path) -> Vec<Skill> {
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};
    type Cache = Option<(std::path::PathBuf, Instant, Vec<Skill>)>;
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(|| Mutex::new(None)).lock().unwrap();
    if let Some((path, timestamp, skills)) = cache.as_ref() {
        if path == workspace && timestamp.elapsed() < Duration::from_secs(1) { return skills.clone(); }
    }
    let skills = discover_skills(workspace);
    *cache = Some((workspace.to_path_buf(), Instant::now(), skills.clone()));
    skills
}

pub fn discover_skills(workspace_root: &Path) -> Vec<Skill> {
    let mut roots = [".takiza/skills", ".agents/skills", ".skills", ".codex/skills", ".claude/skills"]
        .into_iter().map(|path| (workspace_root.join(path), true)).collect::<Vec<_>>();
    if let Some(home) = crate::theme::dirs_next_or_home() {
        for path in [".takiza/skills", ".config/takiza/skills", ".agents/skills",
            ".codex/skills", ".claude/skills", ".gemini/config/skills",
            ".gemini/antigravity-cli/builtin/skills"] {
            roots.push((home.join(path), false));
        }
    }
    if let Some(home) = std::env::var_os("CODEX_HOME") {
        roots.push((std::path::PathBuf::from(home).join("skills"), false));
    }
    discover_from_roots(&roots)
}

fn discover_from_roots(roots: &[(std::path::PathBuf, bool)]) -> Vec<Skill> {
    let mut skills = Vec::new();
    let mut seen_names = std::collections::HashSet::new();
    let mut visited = std::collections::HashSet::new();
    for (root, local) in roots {
        scan_skills_in_dir(root, *local, 0, &mut skills, &mut seen_names, &mut visited);
    }
    skills.sort_by(|a, b| b.is_workspace.cmp(&a.is_workspace)
        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    skills
}

fn scan_skills_in_dir(
    dir: &Path,
    is_workspace: bool,
    depth: usize,
    skills: &mut Vec<Skill>,
    seen_names: &mut std::collections::HashSet<String>,
    visited: &mut std::collections::HashSet<std::path::PathBuf>,
) {
    if depth > 8 { return; }
    let Ok(canonical) = fs::canonicalize(dir) else { return; };
    if !canonical.is_dir() || !visited.insert(canonical.clone()) { return; }
    let skill_md = canonical.join("SKILL.md");
    if let Some(skill) = parse_skill_file(&skill_md, is_workspace) {
        if seen_names.insert(skill.name.to_lowercase()) { skills.push(skill); }
        return;
    }
    let Ok(entries) = fs::read_dir(&canonical) else { return; };
    let mut paths = entries.flatten().map(|entry| entry.path()).collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        if matches!(path.file_name().and_then(|name| name.to_str()),
            Some(".git" | "node_modules" | "target")) { continue; }
        scan_skills_in_dir(&path, is_workspace, depth + 1, skills, seen_names, visited);
    }
}

pub fn parse_skill_file(skill_md: &Path, is_workspace: bool) -> Option<Skill> {
    let content = fs::read_to_string(skill_md).ok()?;
    let dir = skill_md.parent()?.to_string_lossy().to_string();
    let folder_name = skill_md
        .parent()?
        .file_name()?
        .to_string_lossy()
        .to_string();

    let mut name = folder_name.clone();
    let mut description = String::new();

    // Parse YAML frontmatter if present
    if content.starts_with("---") {
        let after_first = &content[3..];
        if let Some(end_idx) = after_first.find("---") {
            let yaml_block = &after_first[..end_idx];
            let mut in_desc = false;
            let mut desc_lines = Vec::new();

            for line in yaml_block.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("name:") {
                    in_desc = false;
                    name = trimmed[5..].trim().trim_matches('"').trim_matches('\'').to_string();
                } else if trimmed.starts_with("description:") {
                    in_desc = true;
                    let rest = trimmed[12..].trim();
                    let clean = rest
                        .trim_start_matches(">-")
                        .trim_start_matches('>')
                        .trim_start_matches('|')
                        .trim()
                        .trim_matches('"')
                        .trim_matches('\'');
                    if !clean.is_empty() {
                        desc_lines.push(clean.to_string());
                    }
                } else if in_desc {
                    if line.starts_with("  ") || line.starts_with('\t') {
                        let clean = trimmed.trim_matches('"').trim_matches('\'');
                        if !clean.is_empty() {
                            desc_lines.push(clean.to_string());
                        }
                    } else if !trimmed.is_empty() {
                        in_desc = false;
                    }
                }
            }

            if !desc_lines.is_empty() {
                description = desc_lines.join(" ");
            }
        }
    }

    if name.trim().is_empty() { name = folder_name; }

    if description.is_empty() {
        for line in content.lines() {
            let t = line.trim();
            if !t.is_empty() && !t.starts_with('#') && !t.starts_with("---") {
                description = t.to_string();
                break;
            }
        }
        if description.is_empty() {
            description = format!("Skill instructions for {}", name);
        }
    }

    Some(Skill {
        name,
        description,
        path: skill_md.to_string_lossy().to_string(),
        directory: dir,
        is_workspace,
    })
}

pub fn get_skill_content(workspace_root: &Path, name: &str) -> Option<String> {
    let skills = discover_skills(workspace_root);
    let target = skills.into_iter().find(|s| s.name.eq_ignore_ascii_case(name))?;
    fs::read_to_string(Path::new(&target.path)).ok()
}

pub fn format_skills_for_prompt(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    out.push_str("<skills>\n");
    out.push_str("Available specialized skills in this project and system:\n");
    for s in skills {
        out.push_str(&format!(
            "- **{}** (`{}`): {}\n",
            s.name, s.path, s.description
        ));
    }
    out.push_str("\nUsers can explicitly select a skill with $skill-name or a Markdown link to its SKILL.md. These references request that you read and apply that skill.\n");
    out.push_str("\nSKILL USAGE RULES:\n");
    out.push_str("1. Read skills explicitly requested by the user and skills whose descriptions clearly fit the task, using `read_skill` or `read_file` before applying them. Descriptions are not full instructions; do not load unrelated skills based only on shared keywords.\n");
    out.push_str("2. Follow applicable skill instructions within the user's authorized scope. Direct user instructions take precedence over skills and project documents. Resolve routine implementation choices yourself; ask only for necessary missing information or authorization.\n");
    out.push_str("</skills>\n");

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_references_resolve_exact_names_and_paths() {
        let skill = Skill { name: "design".into(), description: "Design".into(),
            path: "/tmp/test/SKILL.md".into(), directory: "/tmp/test".into(), is_workspace: true };
        assert_eq!(skill.prompt_reference(), "$design ");
        let skills = [skill];
        for input in ["$design update UI", "Use ($DESIGN), please", "[design](/tmp/test/SKILL.md)"] {
            assert!(requested_skills_for_prompt(input, &skills).contains("<requested_skills>"));
        }
        for input in ["$designer", "foo$design", "$design-extra", "$unknown", "ordinary text"] {
            assert!(requested_skills_for_prompt(input, &skills).is_empty());
        }
    }

    #[test]
    fn discovers_nested_skills_with_local_priority_and_stable_order() {
        let root = std::env::temp_dir().join(format!("takiza-skills-{}", std::process::id()));
        let local = root.join("local");
        let global = root.join("global");
        for (base, path, name) in [(&local, "nested/shared", "Shared"),
            (&global, "shared", "shared"), (&global, "a", "Alpha")] {
            let directory = base.join(path);
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("SKILL.md"), format!("---\nname: {name}\ndescription: Test skill\n---\nInstructions")).unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&local, local.join("cycle")).unwrap();
        let skills = discover_from_roots(&[(local, true), (global, false), (root.join("missing"), false)]);
        assert_eq!(skills.iter().map(|skill| skill.name.as_str()).collect::<Vec<_>>(), ["Shared", "Alpha"]);
        assert!(skills[0].is_workspace);
        assert!(!skills[1].is_workspace);
        assert!(Path::new(&skills[0].path).is_absolute());
        assert!(format_skills_for_prompt(&skills).contains(&skills[0].path));
        fs::remove_dir_all(root).unwrap();
    }
}
