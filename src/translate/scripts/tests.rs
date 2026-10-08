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
    analyze(script, text, &exists, &|_: &str| Vec::new()).into_iter().map(|c| (c.line, c.text, c.outcome)).collect()
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
        ("postinst", "chmod u+s /opt/App/chrome-sandbox", Some("actions")),
        ("postinst", "chmod -R 755 /opt/App", Some("unknown")),
        ("postinst", "chown root:root /opt/App/chrome-sandbox", Some("handled")),
        ("postinst", "chown nobody:nogroup /var/lib/app", Some("actions")),
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
        ("postinst", "xdg-desktop-menu install /opt/App/app.desktop", Some("actions")),
        ("postinst", "/usr/bin/xdg-desktop-menu install --mode system --novendor /opt/App/app.desktop", Some("actions")),
        ("prerm", "xdg-desktop-menu uninstall app.desktop", Some("handled")),
        ("postinst", "xdg-desktop-menu install --mode user /opt/App/app.desktop", Some("unknown")),
        ("postinst", "xdg-desktop-menu install /opt/App/app.directory", Some("unknown")),
        ("postinst", "xdg-icon-resource install --size 48 /opt/App/icon.png app", Some("actions")),
        ("postinst", "xdg-icon-resource install --noupdate --size ${SIZE} /opt/App/icon.png app", Some("unknown")),
        ("postinst", "xdg-icon-resource install /opt/App/icon.png app", Some("unknown")),
        ("prerm", "xdg-icon-resource uninstall --size 48 app", Some("handled")),
        ("postinst", "chmod g+s /opt/App/app", Some("actions")),
        ("postinst", "chmod q+z /opt/App/app", Some("unknown")),
        ("postinst", "chgrp video /opt/App/app", Some("actions")),
        ("postinst", "chgrp -R video /opt/App", Some("unknown")),
        ("postinst", "chown :root /opt/App/app", Some("handled")),
        ("postinst", "systemctl enable /usr/lib/systemd/system/app.service", Some("service")),
        ("postinst", "sed -i 's|pkill|/usr/bin/pkill|g' /opt/App/app.service", Some("actions")),
        ("postinst", "sed -i.bak 's/a/b/' /opt/App/app.service", Some("unknown")),
        ("postinst", "sed 's/a/b/' /opt/App/app.service", Some("unknown")),
        ("postinst", "sed -i -e 's/a/b/' -e 's/c/d/' /opt/App/x", Some("unknown")),
        ("postinst", "touch /opt/App/.marker", Some("actions")),
        ("postinst", "touch -c /opt/App/.marker", Some("handled")),
        ("postinst", "install -Dm0644 /opt/App/a -t /usr/share/doc/app/", Some("actions")),
        ("postinst", "install -D -m 0755 /opt/App/a /usr/lib/app/a", Some("actions")),
        ("postinst", "getent group app", None),
        ("postinst", "id -u", None),
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

/// Like `run`, with a package that has Spotify-style icons for globs to find.
fn run_globbing(script: &str, text: &str) -> Vec<(usize, String, Outcome)> {
    let exists = |p: &str| match p {
        "/usr/share/spotify" | "/opt/App" => Some(PathFact::Dir),
        _ if p.starts_with("/usr/share/spotify/") || p.starts_with("/opt/App/") => Some(PathFact::File { exec: true, empty: false }),
        _ => None,
    };
    let glob = |pattern: &str| -> Vec<String> {
        ["/usr/share/spotify/icons/spotify-linux-16.png", "/usr/share/spotify/icons/spotify-linux-256.png", "/usr/share/spotify/spotify.desktop"]
            .iter()
            .filter(|p| glob_path(pattern, p))
            .map(|p| p.to_string())
            .collect()
    };
    analyze(script, text, &exists, &glob).into_iter().map(|c| (c.line, c.text, c.outcome)).collect()
}

/// The actions of commands that run for sure.
fn actions(out: &[(usize, String, Outcome)]) -> Vec<Action> {
    out.iter().filter_map(|(_, _, o)| if let Outcome::Actions(a) = o { Some(a.clone()) } else { None }).flatten().collect()
}

fn copy(from: &str, to: &str) -> Action {
    Action::Copy { from: from.into(), to: to.into() }
}

#[test]
fn tool_probes_do_not_hide_the_rest_of_the_script() {
    // Chrome's postinst, cut down: the exit behind `command -v` never runs on Arch.
    let script = r#"
XDG_ICON_RESOURCE="`command -v xdg-icon-resource 2> /dev/null || true`"
if [ ! -x "$XDG_ICON_RESOURCE" ]; then
  echo "Error: Could not find xdg-icon-resource" >&2
  exit 1
fi
for icon in  product_logo_16.png product_logo_256.png; do
  size="$(echo ${icon} | sed 's/[^0-9]//g')"
  "$XDG_ICON_RESOURCE" install --size "${size}" "/opt/App/${icon}" \
    "app"
done
update-alternatives --install /usr/bin/app app /opt/App/app 100
"#;
    let out = run("postinst", script);
    assert!(!out.iter().any(|(_, _, o)| matches!(o, Outcome::Conditional { .. } | Outcome::Unknown(_))), "{out:?}");
    assert_eq!(
        actions(&out),
        [
            copy("/opt/App/product_logo_16.png", "/usr/share/icons/hicolor/16x16/apps/app.png"),
            copy("/opt/App/product_logo_256.png", "/usr/share/icons/hicolor/256x256/apps/app.png"),
            symlink("/usr/bin/app", "/opt/App/app"),
        ]
    );
    // A tool Arch does not have answers "not installed"; an unknown one stays open.
    let out = run("postinst", "if command -v apt-config >/dev/null; then\n  rm -f /usr/bin/app\nfi\nif which pkcheck; then\n  rm -f /usr/bin/x\nfi");
    assert_eq!(out.iter().map(|(_, _, o)| kind(o)).collect::<Vec<_>>(), ["skipped", "conditional"]);
}

#[test]
fn unrolls_loops_over_package_files() {
    // Spotify's postinst, cut down: a glob over the package's icons, ${x##*/} and ${x%.png}.
    let script = r#"
spotifyPath=$PWD
if [ -e /usr/share/spotify ]; then
  spotifyPath=/usr/share/spotify
fi
XDG_ICON_RESOURCE="$(command -v xdg-icon-resource 2>/dev/null)"
for icon in "$spotifyPath"/icons/spotify-linux-*.png; do
  [ -e "$icon" ] || break
  size="${icon##*/spotify-linux-}"
  "$XDG_ICON_RESOURCE" install --noupdate --size "${size%.png}" "$icon" "spotify-client"
done
"$XDG_ICON_RESOURCE" forceupdate
XDG_DESKTOP_MENU="$(command -v xdg-desktop-menu 2>/dev/null)"
"$XDG_DESKTOP_MENU" install --novendor "$spotifyPath/spotify.desktop"
"#;
    let out = run_globbing("postinst", script);
    assert_eq!(
        actions(&out),
        [
            copy("/usr/share/spotify/icons/spotify-linux-16.png", "/usr/share/icons/hicolor/16x16/apps/spotify-client.png"),
            copy("/usr/share/spotify/icons/spotify-linux-256.png", "/usr/share/icons/hicolor/256x256/apps/spotify-client.png"),
            copy("/usr/share/spotify/spotify.desktop", "/usr/share/applications/spotify.desktop"),
        ]
    );
    assert!(out.iter().any(|(_, _, o)| *o == Outcome::Hook("icon cache")));

    // break and continue act on the unrolled loop.
    let out = run("postinst", "for f in a b c; do\n  if [ \"$f\" = b ]; then continue; fi\n  [ \"$f\" = c ] && break\n  rm -f /usr/bin/$f\ndone\nfor x in 1 2; do rm -f /usr/bin/y$x; done");
    let rm = |p: &str| Action::Remove { path: p.into() };
    assert_eq!(actions(&out), [rm("/usr/bin/a"), rm("/usr/bin/y1"), rm("/usr/bin/y2")]);
}

#[test]
fn scripts_run_as_root_from_the_root_folder() {
    // 1Password's postinst, cut down: an `id -u` guard, `cd` and ./paths, getent and chgrp.
    let script = r#"
installFiles() {
  CWD=$(pwd)
  cd /opt/App/
  install -Dm0644 ./policy -t /usr/share/polkit-1/actions/
  chmod 4755 ./chrome-sandbox
  if [ ! "$(getent group "app")" ]; then
    groupadd "app"
  fi
  chgrp app ./helper
  chmod g+s ./helper
  cd "$CWD"
  ln -sf /opt/App/app /usr/bin/app
}
if [ "$(id -u)" -ne 0 ]; then
  echo "You must be running as root"
  exit
fi
installFiles
"#;
    let out = run("postinst", script);
    assert!(!out.iter().any(|(_, _, o)| matches!(o, Outcome::Conditional { .. } | Outcome::Unknown(_))), "{out:?}");
    assert_eq!(
        actions(&out),
        [
            copy("/opt/App/policy", "/usr/share/polkit-1/actions/policy"),
            Action::Chmod { path: "/usr/share/polkit-1/actions/policy".into(), mode: 0o644 },
            Action::Chmod { path: "/opt/App/chrome-sandbox".into(), mode: 0o4755 },
            Action::SystemGroup { name: "app".into() },
            Action::Owner { path: "/opt/App/helper".into(), user: None, group: Some("app".into()) },
            Action::ModeChange { path: "/opt/App/helper".into(), spec: "g+s".into() },
            symlink("/usr/bin/app", "/opt/App/app"),
        ]
    );
}

#[test]
fn systemd_is_the_init_system() {
    // RustDesk's postinst, cut down.
    let script = r#"
if [ "$1" = configure ]; then
	INITSYS=$(ls -al /proc/1/exe | awk -F' ' '{print $NF}' | awk -F'/' '{print $NF}')
	if [ "systemd" == "$INITSYS" ]; then
		mkdir -p /usr/lib/systemd/system/
		cp /opt/App/files/app.service /usr/lib/systemd/system/app.service
		if [ -e /usr/bin/pkill ]; then
			sed -i "s|pkill|/usr/bin/pkill|g" /usr/lib/systemd/system/app.service
		fi
		systemctl enable app
	fi
fi
"#;
    let out = run("postinst", script);
    let a = actions(&out);
    assert_eq!(a.len(), 3, "{a:?}");
    assert_eq!(a[0], Action::Mkdir { path: "/usr/lib/systemd/system".into() });
    assert_eq!(a[1], copy("/opt/App/files/app.service", "/usr/lib/systemd/system/app.service"));
    let Action::Edit { path, sed } = &a[2] else { panic!("{a:?}") };
    assert_eq!(path, "/usr/lib/systemd/system/app.service");
    assert_eq!(sed.apply("ExecStop=pkill -f app"), "ExecStop=/usr/bin/pkill -f app");
    assert!(out.iter().any(|(_, _, o)| matches!(o, Outcome::Service(s) if s[0].unit == "app.service")));
}

#[test]
fn double_bracket_tests_keep_their_operators() {
    let out = run("postrm", "if [[ -f /opt/App/app || -L /opt/App/link ]]; then\n  rm -f /usr/bin/app\nfi");
    assert_eq!(actions(&out), [Action::Remove { path: "/usr/bin/app".into() }]);
    let out = run("postrm", "if [[ -f /opt/App/nothing && -f /opt/App/app ]]; then\n  rm -f /usr/bin/app\nfi");
    assert_eq!(out.iter().map(|(_, _, o)| kind(o)).collect::<Vec<_>>(), ["skipped"]);
    // Old style -a and -o, numbers, and an empty path.
    let cases = [
        ("[ -f /opt/App/app -a -d /opt/App ]", "actions"),
        ("[ -f /opt/App/nothing -o -d /opt/App ]", "actions"),
        ("[ -f /opt/App/nothing -a -d /opt/App ]", "skipped"),
        ("[ 3 -gt 2 ]", "actions"),
        ("[ 0 -ne 0 ]", "skipped"),
        ("[ -x \"\" ]", "skipped"),
    ];
    for (test, want) in cases {
        let out = run("postinst", &format!("if {test}; then rm -f /usr/bin/app; fi"));
        assert_eq!(kind(&out[0].2), want, "{test}");
    }
}

#[test]
fn apt_repository_setup_is_recognized_through_variables() {
    // Chrome's apt functions: the files are named through variables apt-config fills in.
    let script = r#"
APT_CONFIG="$(command -v apt-config 2>/dev/null)"
GPG_FILE="/usr/share/keyrings/app.gpg"
find_apt_sources() {
  eval $("$APT_CONFIG" shell APT_SOURCESDIR 'Dir::Etc::sourceparts/d')
  SOURCES_FILE="$APT_SOURCESDIR/app.sources"
}
find_apt_sources
echo "$KEY" | base64 -d >"$GPG_FILE.$$.tmp"
chmod 644 "$GPG_FILE.$$.tmp"
chmod 644 "$SOURCES_FILE.$$.tmp"
mv "$SOURCES_FILE.$$.tmp" "$SOURCES_FILE"
echo repo_add_once="true" > /etc/default/app
"#;
    let out = run("postinst", script);
    assert!(out.iter().all(|(_, _, o)| *o == Outcome::AptRepo), "{out:?}");
    // The pipeline writing the key counts as one unit: both of its commands.
    assert_eq!(out.len(), 7);

    // 1Password fills a temporary folder first and only then moves into apt's places.
    let script = r#"
installDebChannel() {
  TEMPDIR=$(mktemp -d)
  curl -fsS "$KEY_URL" | gpg --dearmor --output "$TEMPDIR/key.gpg"
  curl -fsSo "$TEMPDIR/key.pol" https://example.com/key.pol
  mkdir -p /etc/debsig/policies/AB/
  mv "$TEMPDIR/key.pol" /etc/debsig/policies/AB/key.pol
  rm -rf "$TEMPDIR"
}
installDebChannel
ln -sf /opt/App/app /usr/bin/app
"#;
    let out = run("postinst", script);
    let kinds: Vec<&str> = out.iter().map(|(_, _, o)| kind(o)).collect();
    assert_eq!(kinds, ["apt", "apt", "apt", "apt", "apt", "apt", "actions"], "{out:?}");
}

#[test]
fn decides_what_dpkg_and_the_shell_would() {
    let decided = |script: &str, test: &str| {
        let out = run(script, &format!("if {test}; then rm -f /usr/bin/app; fi"));
        kind(&out[0].2)
    };
    let cases = [
        // $2 is the version installed before: empty on a first install.
        ("postinst", "[ -z \"$2\" ]", "actions"),
        ("prerm", "[ \"$2\" = \"in-favour\" ]", "skipped"),
        // Mullvad's check for an rpm upgrade step.
        ("prerm", "[[ \"$1\" =~ ^[0-9]+$ ]]", "skipped"),
        ("prerm", "[[ \"$1\" =~ ^re ]]", "actions"),
        ("prerm", "[ \"$1\" -gt 0 ]", "skipped"),
        ("prerm", "[ \"$X\" -gt 0 ]", "conditional"),
    ];
    for (script, test, want) in cases {
        assert_eq!(decided(script, test), want, "{script}: {test}");
    }
    // $@ is the arguments dpkg passes: "remove" for a postrm.
    let out = run("postrm", "case $@ in\n  \"purge\")\n    rm -f /usr/bin/a\n    ;;\n  \"remove\")\n    rm -f /usr/bin/b\n    ;;\nesac");
    let kinds: Vec<&str> = out.iter().map(|(_, _, o)| kind(o)).collect();
    assert_eq!(kinds, ["skipped", "actions"], "{out:?}");
    let out = run("prerm", "pkill -2 -x \"app-gui\" || true");
    assert_eq!(kind(&out[0].2), "handled");
    // A loop over the system's files (Discord's /home/*) stays a maybe.
    let out = run("postinst", "for DIR in /home/*; do\n  rm -rf $DIR/.config/app/Cache\ndone");
    assert_eq!(kind(&out[0].2), "conditional", "{out:?}");
}

#[test]
fn evaluates_known_command_substitutions() {
    let vars = HashMap::from([("PWD".to_string(), "/opt/App".to_string()), ("icon".to_string(), "product_logo_48.png".to_string())]);
    let cases = [
        ("command -v xdg-icon-resource 2>/dev/null || true", Some("/usr/bin/xdg-icon-resource")),
        ("command -v apt-config", Some("")),
        ("which pkcheck", None),
        ("id -u", Some("0")),
        ("whoami", Some("root")),
        ("pwd", Some("/opt/App")),
        ("getent group app", Some("")),
        ("echo ${icon} | sed 's/[^0-9]//g'", Some("48")),
        ("echo $unknown | sed 's/x//'", None),
        ("ls -al /proc/1/exe | awk '{print $NF}'", Some("systemd")),
        ("dirname /opt/App/bin/app", Some("/opt/App/bin")),
        ("basename /opt/App/bin/app", Some("app")),
        ("uname -m", None),
    ];
    for (inner, want) in cases {
        assert_eq!(substitute(inner, &vars).as_deref(), want, "{inner}");
    }
}

#[test]
fn expands_parameter_patterns() {
    let vars = HashMap::from([
        ("icon".to_string(), "/usr/share/spotify/icons/spotify-linux-256.png".to_string()),
        ("empty".to_string(), String::new()),
    ]);
    let cases = [
        ("${icon##*/spotify-linux-}", "256.png"),
        ("${icon#*/}", "usr/share/spotify/icons/spotify-linux-256.png"),
        ("${icon%.png}", "/usr/share/spotify/icons/spotify-linux-256"),
        ("${icon%%/*}", ""),
        ("${icon%/*}", "/usr/share/spotify/icons"),
        ("${icon%.svg}", "/usr/share/spotify/icons/spotify-linux-256.png"),
        ("${empty:-fallback}", "fallback"),
        ("${missing%x}", "${missing%x}"),
    ];
    for (input, want) in cases {
        assert_eq!(tokenize(input, &vars), [Tok::Word(want.to_string())], "{input}");
    }
    assert!(pattern_match("*/spotify-linux-", "/usr/share/spotify/icons/spotify-linux-"));
    assert!(pattern_match("a?c", "abc"));
    assert!(!pattern_match("a?c", "ac"));
    assert!(glob_path("/usr/share/*/icons/*.png", "/usr/share/spotify/icons/x.png"));
    assert!(!glob_path("/usr/share/*.png", "/usr/share/spotify/x.png"));
}
