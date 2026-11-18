//! `hdlsim completions <bash|zsh|fish>` - print a self-generated shell
//! completion script (subcommand names are taken from the COMMANDS registry,
//! so the script never drifts from the binary).

use super::{parse_with, Command, COMMANDS};
use clap::Parser;

#[derive(Parser)]
#[command(about = "print a shell completion script: hdlsim completions <bash|zsh|fish>")]
struct CompletionsArgs {
    /// Shell to generate completions for.
    #[arg(value_parser = ["bash", "zsh", "fish"])]
    shell: String,
}

pub struct Completions;

/// Completion script for `shell` ("bash" | "zsh" | "fish"), or None if the
/// shell is unknown. `cmds` are the subcommand names.
pub fn script(shell: &str, cmds: &[String]) -> Option<String> {
    let list = cmds.join(" ");
    match shell {
        "bash" => Some(format!(
            "# hdlsim bash completion\n_hdlsim() {{\n    local cur cmds\n    cur=\"${{COMP_WORDS[COMP_CWORD]}}\"\n    cmds=\"{}\"\n    if [[ $COMP_CWORD -eq 1 ]]; then\n        COMPREPLY=($(compgen -W \"$cmds\" -- \"$cur\"))\n    else\n        case \"${{COMP_WORDS[1]}}\" in\n            run|debug) COMPREPLY=($(compgen -W \"--top --vcd --hwf --wave --live --port --max-time --checkpoint --restore --coverage --coverage-out --seed -L --config -I -S --lib-root --bundle\" -- \"$cur\")) ;;\n            check|ast|init|project|prove|bundle|regress|deps|do|gen|analyze|doc|lint|fmt) COMPREPLY=($(compgen -W \"-L --config -I -S --lib-root --bundle\" -- \"$cur\")) ;;\n            fmt) COMPREPLY=($(compgen -W \"--check -w\" -- \"$cur\")) ;;\n            lint) COMPREPLY=($(compgen -W \"--fix\" -- \"$cur\")) ;;\n            regress) COMPREPLY=($(compgen -W \"--list --junit --coverage --seed\" -- \"$cur\")) ;;\n            *) COMPREPLY=($(compgen -f -- \"$cur\")) ;;\n        esac\n    fi\n}}\ncomplete -F _hdlsim hdlsim\n",
            list
        )),
        "zsh" => Some(format!(
            "#compdef hdlsim\n# hdlsim zsh completion\n_hdlsim() {{\n    local -a cmds\n    cmds=({})\n    if (( CURRENT == 2 )); then\n        _describe 'hdlsim command' cmds\n    else\n        case $words[2] in\n            run|debug) _values 'flag' --top --vcd --hwf --wave --live --port --max-time --checkpoint --restore --coverage --coverage-out --seed -L --config -I -S --lib-root --bundle ;;\n            check|ast|init|project|prove|bundle|regress|deps|do|gen|analyze|doc|lint|fmt) _values 'flag' -L --config -I -S --lib-root --bundle ;;\n            fmt) _values 'flag' --check -w ;;\n            lint) _values 'flag' --fix ;;\n            regress) _values 'flag' --list --junit --coverage --seed ;;\n            *) _files ;;\n        esac\n    fi\n}}\n_hdlsim \"$@\"\n",
            cmds.iter().map(|c| format!("'{}'", c)).collect::<Vec<_>>().join(" ")
        )),
        "fish" => Some(format!(
            "# hdlsim fish completion\nfor c in {}\n    complete -c hdlsim -n '__fish_use_subcommand' -a $c\nend\ncomplete -c hdlsim -n '__fish_seen_subcommand_from run debug' -l top -r\ncomplete -c hdlsim -n '__fish_seen_subcommand_from run debug' -l vcd -r\ncomplete -c hdlsim -n '__fish_seen_subcommand_from run debug' -l wave -r\ncomplete -c hdlsim -n '__fish_seen_subcommand_from run debug' -l max-time -r\ncomplete -c hdlsim -n '__fish_seen_subcommand_from run debug' -l seed -r\ncomplete -c hdlsim -n '__fish_seen_subcommand_from fmt' -l check\ncomplete -c hdlsim -n '__fish_seen_subcommand_from fmt' -l w\ncomplete -c hdlsim -n '__fish_seen_subcommand_from lint' -l fix\nfor c in run check ast init project prove debug bundle regress deps do gen analyze doc lint fmt\n    complete -c hdlsim -n \"__fish_seen_subcommand_from $c\" -s L -r\n    complete -c hdlsim -n \"__fish_seen_subcommand_from $c\" -l config -r\n    complete -c hdlsim -n \"__fish_seen_subcommand_from $c\" -s I -r\n    complete -c hdlsim -n \"__fish_seen_subcommand_from $c\" -s S -r\n    complete -c hdlsim -n \"__fish_seen_subcommand_from $c\" -l lib-root -r\n    complete -c hdlsim -n \"__fish_seen_subcommand_from $c\" -l bundle -r\nend\n",
            list
        )),
        _ => None,
    }
}

impl Command for Completions {
    fn name(&self) -> &'static str {
        "completions"
    }
    fn about(&self) -> &'static str {
        "print a shell completion script: hdlsim completions <bash|zsh|fish>"
    }
    fn run(&self, args: &[String]) -> anyhow::Result<i32> {
        let a = match parse_with::<CompletionsArgs>("completions", args) {
            Ok(a) => a,
            Err(code) => return Ok(code),
        };
        let shell = &a.shell;
        let cmds: Vec<String> = COMMANDS
            .iter()
            .map(|c| c.name().to_string())
            .filter(|n| n != "--help")
            .collect();
        match script(shell, &cmds) {
            Some(s) => {
                print!("{}", s);
                Ok(0)
            }
            None => Err(anyhow::anyhow!(
                "unknown shell '{}' (expected bash, zsh or fish)",
                shell
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmds() -> Vec<String> {
        vec!["run".to_string(), "lsp".to_string(), "fmt".to_string()]
    }

    #[test]
    fn all_shells_cover_every_command() {
        for shell in ["bash", "zsh", "fish"] {
            let s = script(shell, &cmds()).unwrap();
            for c in cmds() {
                assert!(s.contains(&c), "{} script missing {}", shell, c);
            }
            assert!(s.contains("hdlsim"), "{}", shell);
        }
    }

    #[test]
    fn unknown_shell_is_none() {
        assert!(script("powershell", &cmds()).is_none());
    }
}
