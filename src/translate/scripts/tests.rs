// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn package_paths() -> impl Fn(&str) -> Option<PathFact> {
    |p: &str| match p {
        "/opt/App/chrome-sandbox" | "/opt/App/app" => Some(PathFact::File { exec: true, empty: false }),
        "/usr/lib/app/resources/x.xml" => Some(PathFact::File { exec: false, empty: false }),
        "/opt/App" | "/opt/App/data" => Some(PathFact::Dir),
        "/opt/App/link" => Some(PathFact::Symlink),
        _ if p.starts_with("/opt/App/") || p.starts_with("/usr/lib/app/") => Some(PathFact::Missing),
        _ => None,
    }
}

fn run(script: &str, text: &str) -> Vec<(usize, String, Outcome)> {
    let exists = package_paths();
    analyze(script, text, &exists).into_iter().map(|c| (c.line, c.text, c.outcome)).collect()
}

fn kind(o: &Outcome) -> &'static str {
    match o {
        Outcome::Actions(_) => "actions",
        Outcome::Hook(_) => "hook",
        Outcome::Handled(_) => "handled",
        Outcome::AptRepo => "apt",
        Outcome::Skipped(_) => "skipped",
        Outcome::Conditional { .. } => "conditional",
        Outcome::Service(_) => "service",
        Outcome::Unknown(_) => "unknown",
    }
}

fn symlink(link: &str, target: &str) -> Action {
    Action::Symlink { link: link.into(), target: target.into() }
}

#[test]
fn classifies_single_commands() {
    let cases: Vec<(&str, &str, Option<&str>)> = vec![
        ("postinst", "set -e", None),
        ("postinst", "exit 0", None),
        ("postinst", "#DEBHELPER#", None),
        ("postinst", "echo done", None),
        ("postinst", "update-alternatives --install /usr/bin/app app /opt/App/app 100", Some("actions")),
        ("postinst", "update-alternatives --install /usr/bin/app app $DIR/app 100", Some("unknown")),
        ("postinst", "update-alternatives --install /usr/bin/app app", Some("unknown")),
        ("prerm", "update-alternatives --remove app /opt/App/app", Some("handled")),
        ("postinst", "update-alternatives --set app /opt/App/app", Some("unknown")),
        ("postinst", "chmod 4755 '/opt/App/chrome-sandbox' || true", Some("actions")),
        ("postinst", "chmod u+s /opt/App/chrome-sandbox", Some("unknown")),
        ("postinst", "chmod -R 755 /opt/App", Some("unknown")),
        ("postinst", "chown root:root /opt/App/chrome-sandbox", Some("handled")),
        ("postinst", "chown nobody:nogroup /var/lib/app", Some("unknown")),
        ("postinst", "ln -sf /opt/App/app /usr/bin/app", Some("actions")),
        ("postinst", "ln /opt/App/app /usr/bin/app", Some("unknown")),
        ("postinst", "ln -sr /opt/App/app /usr/bin/app", Some("unknown")),
        ("postrm", "rm -f /usr/bin/app", Some("actions")),
        ("postrm", "rm -rf \"$HOME/.config/app\"", Some("unknown")),
        ("postinst", "cp /opt/App/a.xml /usr/share/mime/packages/", Some("actions")),
        ("postinst", "cp -r /opt/App/dir /usr/share/x", Some("unknown")),
        ("postinst", "mkdir -p /usr/share/mime/packages", Some("actions")),
        ("postinst", "install -m 644 /opt/App/a /usr/share/a", Some("actions")),
        ("postinst", "update-desktop-database -q > /dev/null 2>&1", Some("hook")),
        ("postinst", "gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor || true", Some("hook")),
        ("postinst", "ldconfig", Some("hook")),
        ("postinst", "xdg-icon-resource forceupdate --theme hicolor", Some("hook")),
        ("postinst", "xdg-desktop-menu install /opt/App/app.desktop", Some("unknown")),
        ("postinst", "systemctl daemon-reload", Some("hook")),
        ("postinst", "systemctl enable app.service", Some("service")),
        ("postinst", "deb-systemd-helper enable app.service", Some("service")),
        ("postinst", "deb-systemd-helper --quiet was-enabled app.service", Some("handled")),
        ("postinst", "deb-systemd-invoke start app.service", Some("service")),
        ("postinst", "invoke-rc.d app start", Some("service")),
        ("prerm", "deb-systemd-invoke stop app.service", Some("handled")),
        ("postinst", "update-rc.d app defaults", Some("handled")),
        ("preinst", "dpkg-maintscript-helper rm_conffile /etc/init.d/app -- \"$@\"", Some("handled")),
        ("postinst", "systemctl start 'app@*'", Some("unknown")),
        ("postinst", "adduser --system --home /var/lib/app app", Some("actions")),
        ("postinst", "adduser $OPTS _app_net", Some("actions")),
        ("postinst", "useradd -r -d /var/lib/app -s /usr/bin/nologin -G video,audio app", Some("actions")),
        ("postinst", "addgroup --system appgroup", Some("actions")),
        ("postinst", "adduser app video", Some("actions")),
        ("postinst", "usermod -aG video app", Some("actions")),
        ("postinst", "adduser --system $NAME", Some("unknown")),
        ("postrm", "deluser --quiet app", Some("handled")),
        ("postinst", "echo 'x' > /etc/app.conf", Some("unknown")),
        ("postinst", "apt-get update", Some("apt")),
        ("postinst", "echo deb http://x stable main > /etc/apt/sources.list.d/x.list", Some("apt")),
        ("postinst", "# writes /etc/apt/sources.list.d/x.list", None),
        ("postinst", "SOURCES=/etc/apt/sources.list.d/x.list", None),
    ];
    for (script, line, want) in cases {
        let got = run(script, line);
        match want {
            None => assert!(got.is_empty(), "{line:?}: expected nothing, got {got:?}"),
            Some(want) => {
                assert_eq!(got.len(), 1, "{line:?}: {got:?}");
                assert_eq!(kind(&got[0].2), want, "{line:?}: {:?}", got[0].2);
            }
        }
    }
}

#[test]
fn resolves_variables_defaults_and_dirname() {
    let script = r#"ROOT="${DPKG_ROOT:-}"
DIR="$ROOT/usr/share/mime/packages"
FILE="$DIR/app.xml"
mkdir -p "$(dirname "$FILE")"
cp "$ROOT/usr/lib/app/resources/x.xml" "$FILE"
chmod 0644 "$FILE"
"#;
    let got = run("postinst", script);
    let outcomes: Vec<&Outcome> = got.iter().map(|(_, _, o)| o).collect();
    assert_eq!(
        outcomes,
        [
            &Outcome::Actions(vec![Action::Mkdir { path: "/usr/share/mime/packages".into() }]),
            &Outcome::Actions(vec![Action::Copy {
                from: "/usr/lib/app/resources/x.xml".into(),
                to: "/usr/share/mime/packages/app.xml".into()
            }]),
            &Outcome::Actions(vec![Action::Chmod { path: "/usr/share/mime/packages/app.xml".into(), mode: 0o644 }]),
        ]
    );
}

#[test]
fn follows_case_on_the_dpkg_action() {
    let script = "case \"$1\" in\n  configure)\n    ln -s /opt/App/app /usr/bin/app\n    ;;\n  abort-upgrade|abort-remove)\n    weird-tool\n    ;;\n  *)\n    other-tool\n    ;;\nesac\n";
    let got = run("postinst", script);
    let summary: Vec<(usize, &str)> = got.iter().map(|(n, _, o)| (*n, kind(o))).collect();
    assert_eq!(summary, [(3, "actions"), (6, "skipped"), (9, "skipped")]);
    assert_eq!(got[0].2, Outcome::Actions(vec![symlink("/usr/bin/app", "/opt/App/app")]));
    // In postrm, $1 is "remove", so the configure branch is the one that is skipped.
    let got = run("postrm", script);
    assert_eq!(kind(&got[0].2), "skipped");
}

#[test]
fn decides_conditions_from_the_package() {
    let script = r#"
if [ -x /opt/App/app ]; then
  ln -s /opt/App/app /usr/bin/app
fi
if [ -x /opt/App/missing ]; then
  ln -s /opt/App/missing /usr/bin/missing
elif [ -z "$ROOTX" ]; then
  maybe-tool
else
  chmod 4755 /opt/App/chrome-sandbox
fi
[ -x /opt/App/missing ] || rm -f /usr/bin/stale
if [ -f /etc/apparmor.d/abi/4.0 ]; then
  cat > /etc/apparmor.d/app <<EOF
profile app /opt/App/app {}
EOF
fi
"#;
    let got = run("postinst", script);
    let summary: Vec<(usize, &str)> = got.iter().map(|(n, _, o)| (*n, kind(o))).collect();
    assert_eq!(
        summary,
        [(3, "actions"), (6, "skipped"), (8, "conditional"), (10, "conditional"), (12, "actions"), (14, "conditional")]
    );
    let Outcome::Conditional { condition, would } = &got[5].2 else { unreachable!() };
    assert_eq!(condition, "/etc/apparmor.d/abi/4.0 exists");
    assert!(would.contains("/etc/apparmor.d/app with the script's"), "{would}");
    let Outcome::Skipped(why) = &got[1].2 else { unreachable!() };
    assert!(why.ends_with("false for the package: /opt/App/missing is executable"), "{why}");
    let Outcome::Conditional { condition, .. } = &got[2].2 else { unreachable!() };
    assert_eq!(condition, "'$ROOTX' is empty");
}

#[test]
fn runs_functions_where_they_are_called() {
    let script = r#"APP=/opt/App
never_called() {
  frobnicate
}
setup()
{
  ln -s "$APP/app" /usr/bin/app
  if [ "$1" = "full" ]; then
chmod 4755 "$APP/chrome-sandbox"
  fi
  return 0
  unreachable-tool
}
one_liner() { ldconfig; }
setup full
one_liner
"#;
    let got = run("postinst", script);
    let summary: Vec<(usize, &str)> = got.iter().map(|(n, _, o)| (*n, kind(o))).collect();
    assert_eq!(summary, [(7, "actions"), (9, "actions"), (14, "hook")]);
}

#[test]
fn writes_heredocs() {
    let script = "NAME=demo\ncat > /usr/share/demo/a.conf <<EOF\nname=$NAME\nliteral=\\$HOME\nEOF\ncat > /usr/share/demo/b.conf <<'EOF'\nkeep=$NAME\nEOF\ncat > /usr/share/demo/c.conf <<EOF\nuser=$UNKNOWN\nEOF\n";
    let got = run("postinst", script);
    let writes: Vec<&Outcome> = got.iter().map(|(_, _, o)| o).collect();
    assert_eq!(
        writes[0],
        &Outcome::Actions(vec![Action::Write { path: "/usr/share/demo/a.conf".into(), content: b"name=demo\nliteral=$HOME\n".to_vec() }])
    );
    assert_eq!(
        writes[1],
        &Outcome::Actions(vec![Action::Write { path: "/usr/share/demo/b.conf".into(), content: b"keep=$NAME\n".to_vec() }])
    );
    assert_eq!(kind(writes[2]), "unknown");
}

#[test]
fn early_exit_makes_the_rest_conditional() {
    let script = "if [ ! -d /etc/foo ]; then\n  exit 0\nfi\nln -s /opt/App/app /usr/bin/app\n";
    let got = run("postinst", script);
    assert_eq!(got.len(), 1);
    let Outcome::Conditional { condition, .. } = &got[0].2 else { panic!("{got:?}") };
    assert_eq!(condition, "the exit on line 2 did not stop the script first");

    let got = run("postinst", "exit 0\nln -s /opt/App/app /usr/bin/app\n");
    assert!(got.is_empty(), "{got:?}");
}

#[test]
fn tokenizes() {
    let vars = HashMap::from([("D".to_string(), "/opt/x".to_string()), ("E".to_string(), String::new())]);
    let words = |s: &str| -> Vec<Vec<String>> { split_commands(tokenize(s, &vars)).into_iter().map(|c| c.words).collect() };
    let cases: Vec<(&str, Vec<Vec<&str>>)> = vec![
        ("a b c", vec![vec!["a", "b", "c"]]),
        ("a 'b c' \"d e\"", vec![vec!["a", "b c", "d e"]]),
        ("a; b && c || d | e", vec![vec!["a"], vec!["b"], vec!["c"], vec!["d"], vec!["e"]]),
        ("ls $D ${D}/y \"$D/z\" '$D'", vec![vec!["ls", "/opt/x", "/opt/x/y", "/opt/x/z", "$D"]]),
        ("ls ${E:-/fallback} ${D:-/fallback} ${NOPE:-x}", vec![vec!["ls", "/fallback", "/opt/x", "${NOPE:-x}"]]),
        ("ls $UNKNOWN $(pwd) `pwd` $(dirname /a/b/c)", vec![vec!["ls", "$UNKNOWN", "$(pwd)", "`pwd`", "/a/b"]]),
        ("cmd >/dev/null 2>&1 # note", vec![vec!["cmd"]]),
        ("x)  ;;", vec![vec!["x)"], vec![";;"]]),
    ];
    for (input, want) in cases {
        assert_eq!(words(input), want, "{input:?}");
    }
    let cmds = split_commands(tokenize("x > /etc/f 2>>/var/log/y <in &>/dev/null", &vars));
    assert_eq!(cmds[0].redirects, ["/etc/f", "/var/log/y", "in", "/dev/null"]);
    let seps: Vec<Sep> = split_commands(tokenize("a && b || c; d", &vars)).iter().map(|c| c.sep).collect();
    assert_eq!(seps, [Sep::And, Sep::Or, Sep::Semi, Sep::End]);
}

#[test]
fn reads_users_groups_and_services() {
    let one = |line: &str| run("postinst", line).remove(0).2;
    assert_eq!(
        one("useradd -r -d /var/lib/app -c 'App daemon' -G video,audio app"),
        Outcome::Actions(vec![
            Action::SystemUser { name: "app".into(), home: Some("/var/lib/app".into()), comment: Some("App daemon".into()) },
            Action::GroupMember { user: "app".into(), group: "video".into() },
            Action::GroupMember { user: "app".into(), group: "audio".into() },
        ])
    );
    assert_eq!(
        one("adduser --system --group --no-create-home --gecos \"App\" app"),
        Outcome::Actions(vec![Action::SystemUser { name: "app".into(), home: None, comment: Some("App".into()) }])
    );
    assert_eq!(one("adduser --group appgroup"), Outcome::Actions(vec![Action::SystemGroup { name: "appgroup".into() }]));
    assert_eq!(
        one("systemctl --user enable --now app-env.service"),
        Outcome::Service(vec![
            ServiceStep { verb: "enable".into(), unit: "app-env.service".into(), user: true },
            ServiceStep { verb: "start".into(), unit: "app-env.service".into(), user: true },
        ])
    );
    assert_eq!(one("invoke-rc.d --quiet app restart"), Outcome::Service(vec![ServiceStep { verb: "start".into(), unit: "app.service".into(), user: false }]));
}

#[test]
fn path_tests_respect_file_types() {
    let cases = [
        ("[ -f /opt/App ]", "skipped"),
        ("[ -d /opt/App ]", "actions"),
        ("[ -d /opt/App/app ]", "skipped"),
        ("[ -x /usr/lib/app/resources/x.xml ]", "skipped"),
        ("[ -x /opt/App/app ]", "actions"),
        ("[ -L /opt/App/link ]", "actions"),
        ("[ -f /opt/App/link ]", "conditional"),
        ("[ -e /opt/App/link ]", "actions"),
        ("[ -e /opt/App/nope ]", "skipped"),
    ];
    for (test, want) in cases {
        let got = run("postinst", &format!("if {test}; then ln -s /opt/App/app /usr/bin/app; fi\n"));
        assert_eq!(got.len(), 1, "{test}: {got:?}");
        assert_eq!(kind(&got[0].2), want, "{test}");
    }
}

#[test]
fn joins_multi_line_strings_and_refuses_brace_paths() {
    let script = "MSG=\"\nName: Please log out\nPriority: Medium\n\"\nrm -rf /var/lib/app/crash.{a,b}\n";
    let got = run("postrm", script);
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].0, 5);
    assert_eq!(kind(&got[0].2), "unknown");
    let cases = [("a \"b", true), ("a \"b\"", false), ("it's", true), ("echo # it's", false), ("x='a\"b'", false), ("\"a\\\"b", true)];
    for (input, want) in cases {
        assert_eq!(unclosed_quote(input), want, "{input:?}");
    }
}

#[test]
fn describes_conditions_in_plain_words() {
    let w = |s: &str| -> Vec<String> { s.split(' ').map(String::from).collect() };
    let cases = [
        ("[ -f /etc/x ]", "/etc/x exists"),
        ("[ ! -e /usr/bin/ccd ]", "/usr/bin/ccd does not exist"),
        ("[ -L /usr/bin/ccd ]", "/usr/bin/ccd is a symlink"),
        ("[ -d /etc/apt/sources.list.d ]", "/etc/apt/sources.list.d is a directory"),
        ("command -v aa-enabled", "aa-enabled is installed"),
        ("aa-enabled --quiet", "'aa-enabled --quiet' succeeds"),
        ("! aa-enabled", "not ('aa-enabled' succeeds)"),
        ("test -x /opt/a", "/opt/a is executable"),
    ];
    for (input, want) in cases {
        assert_eq!(humanize(&w(input)), want, "{input:?}");
    }
    let vars = HashMap::from([("L".to_string(), "/usr/bin/ccd".to_string())]);
    let words: Vec<String> = split_commands(tokenize("[ \"$(readlink \"$L\")\" = x ]", &vars)).remove(0).words;
    assert_eq!(humanize(&words), "/usr/bin/ccd points to x");
}

#[test]
fn finds_heredoc_delimiters() {
    let cases = [
        ("cat <<EOF", Some(("EOF", false, false))),
        ("cat > f << 'END'", Some(("END", true, false))),
        ("cat <<-\"X\"", Some(("X", true, true))),
        ("cat <<< word", None),
        ("echo '<<EOF'", None),
        ("# <<EOF", None),
    ];
    for (input, want) in cases {
        let got = heredoc_delimiter(input);
        assert_eq!(got.as_ref().map(|(w, q, t)| (w.as_str(), *q, *t)), want, "{input:?}");
    }
}

#[test]
fn detects_functions_and_globs() {
    let cases = [
        ("setup() {", Some(("setup", None))),
        ("setup ()", Some(("setup", None))),
        ("function setup {", Some(("setup", None))),
        ("one() { ldconfig; }", Some(("one", Some("ldconfig;")))),
        ("echo $(foo)", None),
        ("if [ x ]; then", None),
    ];
    for (input, want) in cases {
        let got = function_start(input);
        assert_eq!(got.as_ref().map(|(n, b)| (n.as_str(), b.as_deref())), want, "{input:?}");
    }
    let globs = [("*", "x", true), ("abort-*", "abort-upgrade", true), ("configure", "configure", true), ("conf*e", "configure", true), ("x", "y", false)];
    for (p, w, want) in globs {
        assert_eq!(glob_match(p, w), want, "{p} vs {w}");
    }
}
