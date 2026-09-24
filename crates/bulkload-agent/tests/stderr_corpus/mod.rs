//! The PR #55 r3–r6 stderr probe corpora (review artifacts `pr55-r5/` and
//! `pr55-r6/redact_probe_r6.rs`, plus the `n6_refused_detail.sh` end-to-end
//! set), ported as data for the R-N121 no-echo tests. Every value is a
//! fixture. Key-shaped headers are assembled at run time so no literal in
//! this file matches a secret scanner's rule.

#![allow(dead_code, clippy::too_many_lines, clippy::format_collect)]

/// One probe: its group, the raw stderr bytes and the secret they carry
/// (empty for the keep cases, which carry none).
pub struct Probe {
    pub group: String,
    pub input: Vec<u8>,
    pub secret: String,
}

/// Every probe of the r3–r6 corpora.
#[must_use]
pub fn corpus() -> Vec<Probe> {
    let mut cases: Vec<(String, Vec<u8>, String)> = Vec::new();
    {
        let d = "-".repeat(5);
        let pk = "PRIVATE";
        let mut add = |group: &str, input: &[u8], secret: &str| {
            cases.push((group.to_owned(), input.to_vec(), secret.to_owned()));
        };
        // r3 probes
        for (i, s) in [
            (
                &b"Authorization: Basic dXNlcjpodW50ZXIy"[..],
                "dXNlcjpodW50ZXIy",
            ),
            (
                b"authorization:Bearer eyJhbGciOiJIUzI1NiJ9.payload",
                "eyJhbGciOiJIUzI1NiJ9.payload",
            ),
            (b"Authorization=Basic dXNlcjpodW50ZXIy", "dXNlcjpodW50ZXIy"),
            (
                b"token:Bearer eyJhbGciOiJIUzI1NiJ9.payload",
                "eyJhbGciOiJIUzI1NiJ9.payload",
            ),
            (b"{\"token\":\"s3cr3tvalue\"}", "s3cr3tvalue"),
            (b"{\"token\": \"s3cr3tvalue\"}", "s3cr3tvalue"),
            (b"{\"access_token\" : \"s3cr3tvalue\"}", "s3cr3tvalue"),
            (
                b"GET https://h/x?access_token=s3cr3tvalue&x=1",
                "s3cr3tvalue",
            ),
            (b"GET https://h/x?sig=s3cr3tvalue&se=1", "s3cr3tvalue"),
            (
                b"GET https://h/x?X-Amz-Signature=deadbeefcafe",
                "deadbeefcafe",
            ),
            (b"pwd=hunter2", "hunter2"),
            (b"pass: hunter2", "hunter2"),
            (b"Cookie: session=hunter2", "hunter2"),
            (b"PRIVATE-TOKEN: glsecretvalue", "glsecretvalue"),
            (b"password:\nhunter2", "hunter2"),
            (b"line one\r\ntoken =\n  s3cr3tvalue", "s3cr3tvalue"),
            (b"https://jess:hun/ter2@example.org/r", "hun/ter2"),
            (b"https://jess:hunter2@example.org/r", "hunter2"),
            (
                "x gh\u{2028}p_abcdef0123456789 y".as_bytes(),
                "abcdef0123456789",
            ),
            (
                "x gh\u{200b}p_abcdef0123456789 y".as_bytes(),
                "abcdef0123456789",
            ),
            (
                "x ghp\u{0085}_abcdef0123456789 y".as_bytes(),
                "abcdef0123456789",
            ),
            (b"secret\xff=hunter2", "hunter2"),
        ] {
            add("r3", i, s);
        }
        add(
            "r3",
            format!("{d}BEGIN OPENSSH {pk} KEY{d} b3BlbnNzaC1rZXktdjEAAAAA").as_bytes(),
            "b3BlbnNzaC1rZXktdjEAAAAA",
        );
        // r4 probes (redact_probe.rs leak list)
        for (i, s) in [
            ("password\u{a0}hunter2".as_bytes(), "hunter2"),
            ("token\u{2003}hunter2".as_bytes(), "hunter2"),
            (
                "Authorization:\u{3000}Bearer\u{3000}eyJhbGciOi.payload".as_bytes(),
                "eyJhbGciOi",
            ),
            (
                "PRIVATE-TOKEN:\u{a0}glsecretvalue".as_bytes(),
                "glsecretvalue",
            ),
            ("pass\u{85}hunter2".as_bytes(), "hunter2"),
            ("password\u{2028}hunter2".as_bytes(), "hunter2"),
            (b"Authorization: Bearer: eyJhbGciOi.payload", "eyJhbGciOi"),
            (b"Authorization: Bearer=eyJhbGciOi.payload", "eyJhbGciOi"),
            (
                b"Authorization: \"Bearer eyJhbGciOi.payload\"",
                "eyJhbGciOi",
            ),
            (
                b"AUTHORIZATION: basic eC1hY2Nlc3MtdG9rZW46Z2hwX3NlY3JldA==",
                "eC1hY2Nlc3MtdG9rZW46Z2hwX3NlY3JldA",
            ),
            (
                b"Authorization\n:\nBearer\neyJhbGciOi.payload",
                "eyJhbGciOi",
            ),
            (b"Author\nization: Bearer eyJhbGciOi.payload", "eyJhbGciOi"),
            (b"https://h/x?auth=s3cr3tvalue", "s3cr3tvalue"),
            (b"https://h/x?key=s3cr3tvalue", "s3cr3tvalue"),
            (b"https://h/x?code=s3cr3tvalue", "s3cr3tvalue"),
            (b"session=s3cr3tvalue", "s3cr3tvalue"),
            (
                b"https://oauth2:glpat_s3cr3tvalue@gitlab.com/r",
                "glpat_s3cr3tvalue",
            ),
            (
                b"https://x-access-token:s3cr3tvalue@github.com/o/r.git",
                "s3cr3tvalue",
            ),
            (b"https://s3cr3tvalue@github.com/o/r.git", "s3cr3tvalue"),
            (b"'https://jess:hunter2@example.org/r'", "hunter2"),
            (
                b"fatal: unable to access 'https://jess:hunter2@example.org/r/': 401",
                "hunter2",
            ),
            (b"ghp\xe2\x80\x8b_abcdef0123456789", "abcdef0123456789"),
            ("gh\u{a0}p_abcdef0123456789".as_bytes(), "abcdef0123456789"),
            (b"gh p_abcdef0123456789", "abcdef0123456789"),
            (b"ssh: ghp_abcdef0123456789", "abcdef0123456789"),
            (b"TOKEN=abc\tdef", "abc"),
            (b"secret\x1b[0m=hunter2", "hunter2"),
            (b"passwd\x00hunter2", "hunter2"),
            (b"password\x7fhunter2", "hunter2"),
            (b"password\x08hunter2", "hunter2"),
            (b"fatal: password\xc2\xa0hunter2", "hunter2"),
            (
                b"Authorization: Bearer: eyJhbGciOiJIUzI1NiJ9sekrit",
                "eyJhbGciOiJIUzI1NiJ9sekrit",
            ),
        ] {
            add("r4", i, s);
        }
        add(
            "r4",
            format!(
                "{d}BEGIN RSA {pk} KEY{d}\r\nMIIEowIBAAKCAQEAxq9Zbody\r\n{d}END RSA {pk} KEY{d}"
            )
            .as_bytes(),
            "MIIEowIBAAKCAQEAxq9Zbody",
        );
        add(
            "r4",
            format!("{d}BEG\nIN RSA {pk} KEY{d}\nMIIEowIBAAKCAQEAxq9Zbody").as_bytes(),
            "MIIEowIBAAKCAQEAxq9Zbody",
        );
        add(
            "r4",
            format!("{d} BEGIN {pk} KEY {d}\nMIIEowIBAAKCAQEAxq9Zbody").as_bytes(),
            "MIIEowIBAAKCAQEAxq9Zbody",
        );
        add(
            "r4",
            format!("{d}BEG\nIN OPENSSH {pk} KEY{d}\nb3BlbnNzaC1rZXktdjEAAAAA\n").as_bytes(),
            "b3BlbnNzaC1rZXktdjEAAAAA",
        );
        // r4 deferred class (base64 with no key word) - informational
        add(
            "r4-deferred",
            b"key material:\nMIIEowIBAAKCAQEAxq9Zbody\n",
            "MIIEowIBAAKCAQEAxq9Zbody",
        );
        add(
            "r4-deferred",
            b"b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQ",
            "b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQ",
        );
        add(
            "r4-deferred",
            b"eC1hY2Nlc3MtdG9rZW46Z2hwX3NlY3JldA==",
            "eC1hY2Nlc3MtdG9rZW46Z2hwX3NlY3JldA",
        );
        // r5 new: separator after a token prefix (whitespace/control now becomes a space)
        for (i, s) in [
            ("ghp_\u{a0}abcdef0123456789".as_bytes(), "abcdef0123456789"),
            (
                "ghp_\u{2028}abcdef0123456789".as_bytes(),
                "abcdef0123456789",
            ),
            ("ghp_\u{85}abcdef0123456789".as_bytes(), "abcdef0123456789"),
            (b"ghp_\x00abcdef0123456789", "abcdef0123456789"),
            (b"ghp_\x1babcdef0123456789", "abcdef0123456789"),
            (b"remote: ghp_ abcdef0123456789", "abcdef0123456789"),
            (b"ghp_\nabcdef0123456789", "abcdef0123456789"),
            ("AKIA\u{a0}IOSFODNN7EXAMPLE".as_bytes(), "IOSFODNN7EXAMPLE"),
            (
                "github_pat_\u{2003}11ABCDEFG0123456789".as_bytes(),
                "11ABCDEFG0123456789",
            ),
            ("glpat-\u{85}s3cr3tvalue0123".as_bytes(), "s3cr3tvalue0123"),
            (
                "xoxb-\u{a0}123456789012-s3cr3tv".as_bytes(),
                "123456789012-s3cr3tv",
            ),
            (b"g h p_abcdef0123456789", "abcdef0123456789"),
            (
                "g\u{a0}h\u{a0}p_abcdef0123456789".as_bytes(),
                "abcdef0123456789",
            ),
        ] {
            add("r5-prefix", i, s);
        }
        // r5 new: short keys (Follow::Separator / Equals) with a spaced separator
        for (i, s) in [
            (&b"pass : hunter2"[..], "hunter2"),
            (b"pwd = hunter2", "hunter2"),
            (b"sig = s3cr3tvalue", "s3cr3tvalue"),
            (b"signature : s3cr3tvalue", "s3cr3tvalue"),
            (b"Cookie : abc123s3cr3t", "abc123s3cr3t"),
            (b"{\"pwd\" : \"hunter2\"}", "hunter2"),
            (b"{\"pass\": \"hunter2\"}", "hunter2"),
            ("pass\u{a0}:\u{a0}hunter2".as_bytes(), "hunter2"),
            (b"pass\n:\nhunter2", "hunter2"),
            (b"session = s3cr3tvalue", "s3cr3tvalue"),
            (b"auth: s3cr3tvalue", "s3cr3tvalue"),
            (b"API key: s3cr3tvalue", "s3cr3tvalue"),
        ] {
            add("r5-spaced-key", i, s);
        }
        // r5 new: misc
        for (i, s) in [
            (&b"https://jess:1234/5678@example.org/r"[..], "1234/5678"),
            (b"https://jess:hun@ter2@example.org/r", "ter2"),
            (
                "-----BE\u{200b}GIN OPENSSH PRIVATE KEY----- b3BlbnNzaC1rZXktdjEAAAAA".as_bytes(),
                "b3BlbnNzaC1rZXktdjEAAAAA",
            ),
            (
                b"- - - - -BEGIN OPENSSH PRIVATE KEY----- b3BlbnNzaC1rZXktdjEAAAAA",
                "b3BlbnNzaC1rZXktdjEAAAAA",
            ),
            (
                b"BEGIN OPENSSH PRIVATE KEY b3BlbnNzaC1rZXktdjEAAAAA",
                "b3BlbnNzaC1rZXktdjEAAAAA",
            ),
            (b"ftp://anonymous:hunter2@h/x", "hunter2"),
            (b"ssh://git:hunter2@h:22/x", "hunter2"),
        ] {
            add("r5-misc", i, s);
        }
        // ---------------- r6 additions (all values are fixtures) ----------------
        // (group, label, input, leak target). The label never contains the target.
        let mut r6: Vec<(String, String, Vec<u8>, String)> = Vec::new();
        let body = "abcdefghij0123456789ABCDEFGHIJklmnop"; // 36 token chars, fixture
        let seps: [(&str, &str); 6] = [
            ("SP", " "),
            ("NBSP", "\u{a0}"),
            ("LF", "\n"),
            ("NUL", "\u{0}"),
            ("LS", "\u{2028}"),
            ("TAB", "\t"),
        ];
        let prefixes = [
            "ghp_",
            "gho_",
            "ghs_",
            "ghu_",
            "github_pat_",
            "glpat-",
            "xoxb-",
            "AKIA",
        ];
        // r6-offset: separator after the whole prefix plus k body characters.
        for p in prefixes {
            for k in 0..=12usize {
                for (sn, s) in seps {
                    let input = format!("remote: {p}{}{s}{}", &body[..k], &body[k..]);
                    r6.push((
                        "r6-offset".into(),
                        format!("{p} k={k} sep={sn}"),
                        input.into_bytes(),
                        body[k..].to_owned(),
                    ));
                }
            }
        }
        // r6-split: prefix split by a separator at every position, body attached;
        // and the same split plus a separator after the prefix; and a 3-way split.
        for p in prefixes {
            for i in 1..p.len() {
                for (sn, s) in seps {
                    let (a, b) = p.split_at(i);
                    r6.push((
                        "r6-split".into(),
                        format!("{p} split@{i} sep={sn}"),
                        format!("x {a}{s}{b}{body} y").into_bytes(),
                        body.to_owned(),
                    ));
                    r6.push((
                        "r6-split+after".into(),
                        format!("{p} split@{i}+after sep={sn}"),
                        format!("x {a}{s}{b}{s}{body} y").into_bytes(),
                        body.to_owned(),
                    ));
                    for j in (i + 1)..p.len() {
                        let (b1, b2) = b.split_at(j - i);
                        r6.push((
                            "r6-split3(deferred)".into(),
                            format!("{p} split@{i},{j} sep={sn}"),
                            format!("x {a}{s}{b1}{s}{b2}{body} y").into_bytes(),
                            body.to_owned(),
                        ));
                    }
                }
            }
        }
        // r6-prefix-context: punctuation before a whole prefix, then a separator.
        for (lbl, pre) in [
            ("colon", "remote:"),
            ("eq", "token_id="),
            ("quote", "'"),
            ("dquote", "\""),
            ("paren", "("),
            ("url", "https://"),
            ("user", "https://x-access-token:"),
        ] {
            for (sn, s) in seps {
                r6.push((
                    "r6-prefix-context".into(),
                    format!("{lbl}+ghp_ sep={sn}"),
                    format!("{pre}ghp_{s}{body}").into_bytes(),
                    body.to_owned(),
                ));
            }
        }
        // r6-key: every short key with each separator form.
        let v = "Zq9s3cr3tVal";
        let short = [
            "pass",
            "pwd",
            "sig",
            "signature",
            "cookie",
            "auth",
            "key",
            "code",
            "session",
            "sid",
        ];
        let any = ["password", "token", "secret", "api_key", "passphrase"];
        let forms: [(&str, &str); 26] = [
            ("K : v", "{K} : {V}"),
            ("K = v", "{K} = {V}"),
            ("K :v", "{K} :{V}"),
            ("K =v", "{K} ={V}"),
            ("K:v", "{K}:{V}"),
            ("K=v", "{K}={V}"),
            ("K: v", "{K}: {V}"),
            ("K= v", "{K}= {V}"),
            ("K NBSP:NBSP v", "{K}\u{a0}:\u{a0}{V}"),
            ("K LF = LF v", "{K}\n=\n{V}"),
            ("K TAB : TAB v", "{K}\t:\t{V}"),
            ("json spaced", "{\"{K}\" : \"{V}\"}"),
            ("json LF", "{\n  \"{K}\"\n  :\n  \"{V}\"\n}"),
            ("json padded", "{ \"{K}\" : \"{V}\" }"),
            (
                "json compact multi, spaced colon",
                "{\"u\":\"a\",\"{K}\" : \"{V}\"}",
            ),
            ("json spaced multi", "{\"u\": \"a\", \"{K}\" : \"{V}\"}"),
            ("json compact multi", "{\"u\":\"a\",\"{K}\":\"{V}\"}"),
            (
                "json compact multi colon-space",
                "{\"u\":\"a\",\"{K}\": \"{V}\"}",
            ),
            ("single-quoted", "'{K}' : '{V}'"),
            ("K := v", "{K} := {V}"),
            ("K => v", "{K} => {V}"),
            ("ruby hash", "{\"{K}\" => \"{V}\"}"),
            ("--K = v", "--{K} = {V}"),
            ("$K = v", "${K} = {V}"),
            ("K fullwidth-colon v", "{K} \u{ff1a} {V}"),
            ("upper K = v", "{U} = {V}"),
        ];
        for k in short.iter().chain(any.iter()) {
            for (fl, f) in forms {
                let input = f
                    .replace("{K}", k)
                    .replace("{U}", &k.to_uppercase())
                    .replace("{V}", v);
                let grp = if any.contains(k) {
                    "r6-key-any"
                } else {
                    "r6-key-short"
                };
                r6.push((
                    grp.into(),
                    format!("{k}: {fl}"),
                    input.into_bytes(),
                    v.to_owned(),
                ));
            }
        }
        // r6-multiline: several secrets on several lines, keys split across lines.
        for (lbl, input) in [
            (
                "3 lines, 3 secrets",
                format!("fatal: x\nremote: pass : {v}\nremote: pwd = {v}\n"),
            ),
            ("CRLF spaced pwd", format!("line1\r\npwd\r\n=\r\n{v}\r\n")),
            (
                "prefix at EOL, body next line",
                format!("remote: ghp_\n{v}\nfatal: y"),
            ),
            ("prefix+4 at EOL", format!("remote: ghp_abcd\n{v}")),
            ("prefix+5 at EOL", format!("remote: ghp_abcde\n{v}")),
            ("key word split: pass/word:", format!("pass\nword: {v}")),
            ("key word split: pass/word =", format!("pass\nword = {v}")),
            ("key word split: tok/en:", format!("tok\nen: {v}")),
            ("key word split: pw/d =", format!("pw\nd = {v}")),
            (
                "value of key is a key: token pwd = v",
                format!("token pwd = {v}"),
            ),
            (
                "value of key is a key: password: pass : v",
                format!("password: pass : {v}"),
            ),
            ("two spaced keys adjacent", format!("pass : {v} pwd = {v}")),
            (
                "long line: secret past 240 chars",
                format!("{} pass : {v}", "x".repeat(250)),
            ),
        ] {
            r6.push((
                "r6-multiline".into(),
                lbl.into(),
                input.into_bytes(),
                v.to_owned(),
            ));
        }

        for (group, _, input, secret) in r6 {
            add(&group, &input, &secret);
        }
    }
    cases.extend(end_to_end());
    cases
        .into_iter()
        .map(|(group, input, secret)| Probe {
            group,
            input,
            secret,
        })
        .collect()
}

/// The `n6_refused_detail.sh` end-to-end set, keep cases included.
fn end_to_end() -> Vec<(String, Vec<u8>, String)> {
    let d = "-".repeat(5);
    let pk = "PRIVATE";
    let b = "abcdefghij0123456789ABCDEFGHIJklmnop";
    let v = "Zq9s3cr3tVal";
    let mut cases: Vec<(String, Vec<u8>, String)> = vec![
        ("e2e".into(), b"fatal: password\xc2\xa0hunter2\n".to_vec(), "hunter2".into()),
        ("e2e".into(), b"token\xe2\x80\x83hunter2\n".to_vec(), "hunter2".into()),
        ("e2e".into(), b"passwd\x00hunter2\n".to_vec(), "hunter2".into()),
        ("e2e".into(), b"password\x08hunter2\n".to_vec(), "hunter2".into()),
        (
            "e2e".into(),
            b"Authorization: Bearer: eyJhbGciOiJIUzI1NiJ9sekrit\n".to_vec(),
            "eyJhbGciOiJIUzI1NiJ9sekrit".into(),
        ),
        (
            "e2e".into(),
            format!("{d}BEGIN OPENSSH {pk} KEY{d}\r\nb3BlbnNzaC1rZXktdjEAAAAA\r\n{d}END OPENSSH {pk} KEY{d}\r\n").into_bytes(),
            "b3BlbnNzaC1rZXktdjEAAAAA".into(),
        ),
        (
            "e2e".into(),
            format!("{d}BEG\nIN OPENSSH {pk} KEY{d}\nb3BlbnNzaC1rZXktdjEAAAAA\n").into_bytes(),
            "b3BlbnNzaC1rZXktdjEAAAAA".into(),
        ),
        ("e2e".into(), format!("remote: ghp_{}\n", &b[..22]).into_bytes(), b[..22].into()),
        (
            "e2e".into(),
            b"fatal: unable to access https://jess:hun/ter2@example.org/r/\n".to_vec(),
            "hun/ter2".into(),
        ),
        ("e2e".into(), b"pass\xc2\x85hunter2\n".to_vec(), "hunter2".into()),
        ("e2e".into(), format!("remote: ghp_\u{a0}{b}\n").into_bytes(), b.into()),
        ("e2e".into(), "AKIA\u{a0}IOSFODNN7EXAMPLE\n".as_bytes().to_vec(), "IOSFODNN7EXAMPLE".into()),
        ("e2e".into(), format!("ghp_\u{0}{b}\n").into_bytes(), b.into()),
        ("e2e".into(), format!("ghp_\n{b}\n").into_bytes(), b.into()),
        ("e2e".into(), format!("glpat-\u{85}{b}\n").into_bytes(), b.into()),
        ("e2e".into(), format!("pwd = {v}\n").into_bytes(), v.into()),
        ("e2e".into(), format!("pass : {v}\n").into_bytes(), v.into()),
        ("e2e".into(), format!("{{\"pwd\" : \"{v}\"}}\n").into_bytes(), v.into()),
        ("e2e".into(), format!("auth: {v}\n").into_bytes(), v.into()),
        ("e2e".into(), format!("API key: {v}\n").into_bytes(), v.into()),
        ("e2e".into(), format!("remote: ghp_abcd\u{a0}{}\n", &b[4..]).into_bytes(), b[4..].into()),
        ("e2e".into(), format!("remote: ghp_abcde\u{a0}{}\n", &b[5..]).into_bytes(), b[5..].into()),
        ("e2e".into(), format!("remote: ghp_abcde\n{}\n", &b[5..]).into_bytes(), b[5..].into()),
        ("e2e".into(), format!("remote: ghp_abcdefghij {}\n", &b[10..]).into_bytes(), b[10..].into()),
        ("e2e".into(), format!("xoxb-abc\u{a0}{}\n", &b[3..]).into_bytes(), b[3..].into()),
        ("e2e".into(), format!("gh\u{a0}p_\u{a0}{b}\n").into_bytes(), b.into()),
        ("e2e".into(), format!("$pwd = {v}\n").into_bytes(), v.into()),
        ("e2e".into(), format!("--pass = {v}\n").into_bytes(), v.into()),
        ("e2e".into(), format!("{{\"u\":\"a\",\"pwd\" : \"{v}\"}}\n").into_bytes(), v.into()),
        ("e2e".into(), format!("password => {v}\n").into_bytes(), v.into()),
        ("e2e".into(), format!("pass\nword: {v}\n").into_bytes(), v.into()),
        ("e2e".into(), format!("fatal: x\nremote: pass : {v}\nremote: pwd = {v}\n").into_bytes(), v.into()),
    ];
    for keep in [
        "hint: pass --force to override\n",
        "error: no signature found\n",
        "gpg: Signature made Tue 23 Sep\n",
        "API key rotation scheduled\n",
        "auth: ok\n",
        "fatal: couldn't find remote ref refs/heads/main\n",
        "error: invalid key: remote.origin.url\n",
        "error: exit code=128\n",
        "fatal: repository 'https://h.example:443/@scope/pkg' not found\n",
        "ssh: connect to host sting port 22: Connection timed out\n",
    ] {
        cases.push(("e2e-keep".into(), keep.as_bytes().to_vec(), String::new()));
    }
    cases
}
