//! Tab completion, like apt's: `pacdeb completions <fish|bash|zsh>` prints a script for
//! the shell, and the scripts call `pacdeb __complete <what>` for names that change
//! (tracked apps, saved repositories, installable packages).

use std::collections::BTreeSet;

use crate::error::{Result, bail};
use crate::paths::Paths;
use crate::registry::{Config, presets};
use crate::sources::apt_fetch;

/// Commands with a short description, for shells that show one.
const COMMANDS: &[(&str, &str)] = &[
    ("update", "Check every source for new versions"),
    ("upgrade", "Build and install everything newer"),
    ("install", "Install by name or from a .deb file"),
    ("remove", "Uninstall apps and stop tracking them"),
    ("untrack", "Stop tracking an app, keep it installed"),
    ("search", "Search the saved apt repositories"),
    ("list", "Show tracked apps"),
    ("add", "Track an app"),
    ("set", "Change a tracked app or the global channel"),
    ("check", "Ask the sources now without remembering"),
    ("apt", "Manage saved apt repositories"),
    ("packages", "List everything in an apt repository"),
    ("inspect", "Show what is in a .deb"),
    ("convert", "Build a package without installing it"),
    ("timer", "Scheduled update checks"),
    ("repo", "The local pacman repository"),
    ("help", "Show help for a command"),
];

const APT_COMMANDS: &[&str] = &["list", "add", "show", "edit", "key", "remove", "packages"];

/// `pacdeb __complete <what>`: one candidate per line. Never fails loudly; a shell
/// waiting for completions should just get fewer of them.
pub fn complete(what: &str) -> Result<()> {
    let Ok(paths) = Paths::from_env() else {
        return Ok(());
    };
    let config = Config::load(&paths.config).unwrap_or_default();
    let mut out: BTreeSet<String> = BTreeSet::new();
    match what {
        "apps" => {
            out.extend(config.apps.keys().cloned());
            out.extend(config.apps.values().filter_map(|a| a.pkgname.clone()));
        }
        "repos" => out.extend(config.apt.keys().cloned()),
        "presets" => out.extend(presets().into_keys().filter(|p| !config.apps.contains_key(p))),
        "installable" => {
            out.extend(config.apps.keys().cloned());
            out.extend(presets().into_keys());
            // Only the lists already on disk: completing must not wait for the network.
            for name in config.apt.keys() {
                for (_, index) in apt_fetch::cached_indexes(name, &paths.cache) {
                    for line in index.lines() {
                        if let Some(p) = line.strip_prefix("Package: ") {
                            out.insert(p.trim().to_string());
                        }
                    }
                }
            }
        }
        "commands" => out.extend(COMMANDS.iter().map(|(c, _)| c.to_string())),
        _ => {}
    }
    for c in out {
        println!("{c}");
    }
    Ok(())
}

/// `pacdeb completions <shell>`: the completion script for that shell.
pub fn script(shell: &str) -> Result<()> {
    let text = match shell {
        "fish" => fish(),
        "bash" => BASH.to_string(),
        "zsh" => zsh(),
        other => bail!("no completions for {other}; pick fish, bash or zsh"),
    };
    print!("{text}");
    Ok(())
}

fn fish() -> String {
    let mut s = String::from(
        "# pacdeb completions for fish. Installed by setup.sh; made by 'pacdeb completions fish'.\n\
         function __pacdeb_names\n    command pacdeb __complete $argv 2>/dev/null\nend\n\
         function __pacdeb_after\n    # True when the word before the one being completed is the given command path.\n    set -l words (commandline -opc)\n    test (count $words) -ge 2; or return 1\n    test \"$words[2]\" = \"$argv[1]\"; or return 1\n    test (count $argv) -lt 2; and return 0\n    test (count $words) -ge 3; and test \"$words[3]\" = \"$argv[2]\"\nend\n\
         function __pacdeb_at\n    # True when completing argument number $argv[1] (1 = the command).\n    test (count (commandline -opc)) -eq $argv[1]\nend\n\n\
         complete -c pacdeb -f\n",
    );
    for (cmd, desc) in COMMANDS {
        s.push_str(&format!("complete -c pacdeb -n '__pacdeb_at 1' -a {cmd} -d '{desc}'\n"));
    }
    s.push_str(
        "\n# install: names (tracked, built in, from the saved repositories) and .deb files\n\
         complete -c pacdeb -n '__pacdeb_after install' -a '(__pacdeb_names installable)'\n\
         complete -c pacdeb -n '__pacdeb_after install' -a '(__fish_complete_suffix .deb)'\n\
         complete -c pacdeb -n '__pacdeb_after install' -l direct -d 'Write the package directly instead of running makepkg'\n\
         complete -c pacdeb -n '__pacdeb_after inspect; or __pacdeb_after convert' -a '(__fish_complete_suffix .deb)'\n\
         complete -c pacdeb -n '__pacdeb_after convert' -l dry-run -d 'Show what would be built'\n\
         complete -c pacdeb -n '__pacdeb_after convert' -l direct -d 'Write the package directly'\n\
         complete -c pacdeb -n '__pacdeb_after convert' -l out -r -F -d 'Folder for the package'\n\
         \n# apps\n\
         complete -c pacdeb -n '__pacdeb_after remove; or __pacdeb_after untrack; or __pacdeb_after upgrade; or __pacdeb_after check; or __pacdeb_after set' -a '(__pacdeb_names apps)'\n\
         complete -c pacdeb -n '__pacdeb_after upgrade' -l no-install -d 'Build only'\n\
         complete -c pacdeb -n '__pacdeb_after upgrade' -l direct -d 'Write packages directly'\n\
         complete -c pacdeb -n '__pacdeb_after upgrade' -l file -r -a '(__fish_complete_suffix .deb)' -d 'Use this deb'\n\
         complete -c pacdeb -n '__pacdeb_after list' -l upgradable -d 'Only apps with updates'\n\
         complete -c pacdeb -n '__pacdeb_after add' -a '(__pacdeb_names presets)'\n\
         complete -c pacdeb -n '__pacdeb_after add; or __pacdeb_after set' -l apt -r -a '(__pacdeb_names repos)' -d 'Saved apt repository'\n\
         complete -c pacdeb -n '__pacdeb_after add; or __pacdeb_after set' -l source -r -a 'direct apt github manual' -d 'Source type'\n\
         complete -c pacdeb -n '__pacdeb_after add; or __pacdeb_after set' -l channel -r -d 'Release channel'\n\
         complete -c pacdeb -n '__pacdeb_after add; or __pacdeb_after set' -l package -r -d 'Debian package name'\n\
         complete -c pacdeb -n '__pacdeb_after add; or __pacdeb_after set' -l pkgname -r -d 'Package name to build'\n\
         complete -c pacdeb -n '__pacdeb_after packages' -a '(__pacdeb_names apps; __pacdeb_names repos)'\n\
         \n# apt repositories\n",
    );
    s.push_str(&format!("complete -c pacdeb -n '__pacdeb_after apt; and __pacdeb_at 2' -a '{}'\n", APT_COMMANDS.join(" ")));
    s.push_str(
        "complete -c pacdeb -n '__pacdeb_after apt show; or __pacdeb_after apt edit; or __pacdeb_after apt key; or __pacdeb_after apt remove; or __pacdeb_after apt packages' -a '(__pacdeb_names repos)'\n\
         complete -c pacdeb -n '__pacdeb_after apt add' -l line -r -d 'The vendor\\'s deb line'\n\
         complete -c pacdeb -n '__pacdeb_after apt add' -l file -r -F -d 'A .list or .sources file'\n\
         complete -c pacdeb -n '__pacdeb_after apt add; or __pacdeb_after apt key' -l key-url -r -d 'Signing key link'\n\
         complete -c pacdeb -n '__pacdeb_after apt add; or __pacdeb_after apt key' -l key-fingerprint -r -d 'Pin the key'\n\
         complete -c pacdeb -n '__pacdeb_after apt add; or __pacdeb_after apt key' -l key -r -F -d 'Key file'\n\
         complete -c pacdeb -n '__pacdeb_after apt edit' -l url -r\n\
         complete -c pacdeb -n '__pacdeb_after apt edit' -l suite -r\n\
         complete -c pacdeb -n '__pacdeb_after apt edit' -l components -r\n\
         complete -c pacdeb -n '__pacdeb_after apt edit' -l arch -r\n\
         complete -c pacdeb -n '__pacdeb_after apt remove' -l with-apps -d 'Stop tracking its apps too'\n\
         complete -c pacdeb -n '__pacdeb_after apt show' -l offline -d 'Use the last check'\n\
         \n# the rest\n\
         complete -c pacdeb -n '__pacdeb_after timer; and __pacdeb_at 2' -a 'enable disable status run'\n\
         complete -c pacdeb -n '__pacdeb_after repo; and __pacdeb_at 2' -a 'init status remove'\n\
         complete -c pacdeb -n '__pacdeb_after help; and __pacdeb_at 2' -a '(__pacdeb_names commands)'\n",
    );
    s
}

const BASH: &str = r#"# pacdeb completions for bash. Installed by setup.sh; made by 'pacdeb completions bash'.
_pacdeb_names() { command pacdeb __complete "$1" 2>/dev/null; }

_pacdeb() {
    local cur=${COMP_WORDS[COMP_CWORD]}
    local cmd=${COMP_WORDS[1]}
    local words=""
    COMPREPLY=()
    if [[ $COMP_CWORD -eq 1 ]]; then
        words=$(_pacdeb_names commands)
        COMPREPLY=($(compgen -W "$words" -- "$cur"))
        return
    fi
    local debs=0
    case $cmd in
        install)
            if [[ $cur == -* ]]; then words="--direct"; else words=$(_pacdeb_names installable); debs=1; fi ;;
        inspect) debs=1 ;;
        convert)
            if [[ $cur == -* ]]; then words="--dry-run --direct --out"; else debs=1; fi ;;
        remove|untrack|check|set)
            words=$(_pacdeb_names apps) ;;
        upgrade)
            if [[ $cur == -* ]]; then words="--file --direct --no-install"; else words=$(_pacdeb_names apps); fi ;;
        list) words="--upgradable" ;;
        add)
            if [[ $cur == -* ]]; then words="--apt --source --channel --package --pkgname --preset"; else words=$(_pacdeb_names presets); fi ;;
        packages) words="$(_pacdeb_names apps) $(_pacdeb_names repos)" ;;
        apt)
            if [[ $COMP_CWORD -eq 2 ]]; then
                words="list add show edit key remove packages"
            else
                case ${COMP_WORDS[2]} in
                    show|edit|key|remove|packages)
                        if [[ $cur == -* ]]; then words="--url --suite --components --arch --key-url --key-fingerprint --key --with-apps --offline"
                        else words=$(_pacdeb_names repos); fi ;;
                    add) words="--line --file --key-url --key-fingerprint --key --arch --components" ;;
                esac
            fi ;;
        timer) [[ $COMP_CWORD -eq 2 ]] && words="enable disable status run" ;;
        repo) [[ $COMP_CWORD -eq 2 ]] && words="init status remove" ;;
        help) [[ $COMP_CWORD -eq 2 ]] && words=$(_pacdeb_names commands) ;;
    esac
    # After --apt, a repository name; after --file, a .deb.
    case ${COMP_WORDS[COMP_CWORD-1]} in
        --apt) words=$(_pacdeb_names repos); debs=0 ;;
        --file) words=""; debs=1 ;;
    esac
    COMPREPLY=($(compgen -W "$words" -- "$cur"))
    if [[ $debs -eq 1 ]]; then
        COMPREPLY+=($(compgen -f -X '!*.deb' -- "$cur") $(compgen -d -- "$cur"))
    fi
}
complete -F _pacdeb pacdeb
"#;

fn zsh() -> String {
    let described: Vec<String> = COMMANDS.iter().map(|(c, d)| format!("    '{c}:{d}'")).collect();
    format!(
        "#compdef pacdeb\n# pacdeb completions for zsh. Installed by setup.sh; made by 'pacdeb completions zsh'.\n\
         _pacdeb_names() {{ command pacdeb __complete \"$1\" 2>/dev/null }}\n\n\
         _pacdeb() {{\n  local -a cmds\n  cmds=(\n{}\n  )\n\
         \x20 if (( CURRENT == 2 )); then\n    _describe 'command' cmds\n    return\n  fi\n\
         \x20 case $words[2] in\n\
         \x20   install) compadd -- ${{(f)\"$(_pacdeb_names installable)\"}}; _files -g '*.deb' ;;\n\
         \x20   inspect|convert) _files -g '*.deb' ;;\n\
         \x20   remove|untrack|check|set|upgrade) compadd -- ${{(f)\"$(_pacdeb_names apps)\"}} ;;\n\
         \x20   add) compadd -- ${{(f)\"$(_pacdeb_names presets)\"}} ;;\n\
         \x20   list) compadd -- --upgradable ;;\n\
         \x20   packages) compadd -- ${{(f)\"$(_pacdeb_names apps)\"}} ${{(f)\"$(_pacdeb_names repos)\"}} ;;\n\
         \x20   apt)\n      if (( CURRENT == 3 )); then compadd -- {}\n      else compadd -- ${{(f)\"$(_pacdeb_names repos)\"}}; fi ;;\n\
         \x20   timer) (( CURRENT == 3 )) && compadd -- enable disable status run ;;\n\
         \x20   repo) (( CURRENT == 3 )) && compadd -- init status remove ;;\n\
         \x20   help) (( CURRENT == 3 )) && compadd -- ${{(f)\"$(_pacdeb_names commands)\"}} ;;\n\
         \x20 esac\n}}\n\n_pacdeb \"$@\"\n",
        described.join("\n"),
        APT_COMMANDS.join(" ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_is_completed() {
        for (cmd, _) in COMMANDS {
            assert!(crate::help::page(cmd).is_some() || *cmd == "help", "{cmd} has no help page");
            assert!(fish().contains(&format!("-a {cmd} ")), "fish misses {cmd}");
            assert!(zsh().contains(&format!("'{cmd}:")), "zsh misses {cmd}");
        }
        for (cmd, _) in COMMANDS.iter().filter(|(c, _)| *c != "help") {
            assert!(crate::help::OVERVIEW.contains(&format!("  {cmd} ")), "{cmd} is not in the overview");
        }
    }
}
