//! sidekick: one herdr plugin for a coding-agent workspace. Three side panels share one
//! binary: worktrees (board), changes (+ diff overlay) and agents (+ transcript panes).
//! Usage: sidekick <board|changes|diff|agents|view|notify> [args]   (`--snapshot WxH` works
//! on every pane; `board --status` prints the tab-bar summary).

mod agents;
mod changes;
mod files;
mod highlight;
mod tui;
mod worktrees;

/// Must match `id` in herdr-plugin.toml: some commands run outside the plugin env (the
/// tab-bar `--status` command from herdr's config), so the id cannot come from HERDR_PLUGIN_ID.
pub const PLUGIN_ID: &str = "minhtran3124.sidekick";

fn main() -> std::io::Result<()> {
    let mut argv = std::env::args().skip(1);
    let cmd = argv.next().unwrap_or_default();
    let args: Vec<String> = argv.collect();
    match cmd.as_str() {
        "board" => worktrees::main(args),
        "changes" => changes::list_main(args),
        "diff" => changes::diff_main(args),
        "agents" => agents::list_main(args),
        "view" => agents::view_main(args),
        "notify" => agents::notify_main(),
        "open" => files::open_main(args),
        "--version" | "-V" => {
            println!("sidekick {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            eprintln!("usage: sidekick <board|changes|diff|agents|view|notify|open> [--snapshot WxH]");
            std::process::exit(2);
        }
    }
}

/// The GitHub CLI to call: $SIDEKICK_GH (or the older $WORKTREES_GH), else the first `gh` on
/// PATH that is not a pyenv shim (those reject `--jq`), else common install locations.
pub fn find_gh() -> String {
    if let Some(gh) = ["SIDEKICK_GH", "WORKTREES_GH"].iter().find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty())) {
        return gh;
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let on_path = std::env::split_paths(&path).filter(|d| !d.to_string_lossy().contains("pyenv")).map(|d| d.join("gh"));
    let known = ["/opt/homebrew/bin/gh", "/usr/local/bin/gh", "/usr/bin/gh"].map(std::path::PathBuf::from);
    on_path
        .chain(known)
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "gh".into())
}
