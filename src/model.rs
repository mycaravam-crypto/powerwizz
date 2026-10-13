use reqwest::Url;
use serde::{Deserialize, Serialize};

// ponytail: conservative caps; 500 VUs measured fine (README), raise only on concrete need.
pub const MAX_USERS: u32 = 500;
pub const MAX_DURATION_SECS: u64 = 3600;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
}

impl Method {
    pub const ALL: [Method; 6] = [
        Method::Get,
        Method::Post,
        Method::Put,
        Method::Patch,
        Method::Delete,
        Method::Head,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
            Method::Head => "HEAD",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Step {
    pub name: String,
    pub method: Method,
    /// Absolute URL or path relative to the base URL.
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub pause_min_ms: u64,
    pub pause_max_ms: u64,
    /// Empty means any 2xx.
    pub expected_status: Vec<u16>,
}

impl Default for Step {
    fn default() -> Self {
        Step {
            name: "step".into(),
            method: Method::Get,
            url: "/".into(),
            headers: Vec::new(),
            body: String::new(),
            pause_min_ms: 0,
            pause_max_ms: 0,
            expected_status: Vec::new(),
        }
    }
}

impl Step {
    pub fn status_ok(&self, status: u16) -> bool {
        if self.expected_status.is_empty() {
            (200..300).contains(&status)
        } else {
            self.expected_status.contains(&status)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Scenario {
    pub name: String,
    pub steps: Vec<Step>,
}

impl Default for Scenario {
    fn default() -> Self {
        Scenario {
            name: "scenario".into(),
            steps: vec![Step::default()],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnError {
    /// Count the error, skip the rest of this iteration, start the next one.
    #[default]
    AbortIteration,
    /// Count the error and end this virtual user.
    StopUser,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthStrategy {
    #[default]
    None,
    /// All virtual users log in with the same account.
    Shared,
    /// Virtual user i uses row i of the imported accounts CSV.
    PerUser,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Account {
    pub username: String,
    pub password: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TestDefinition {
    pub base_url: String,
    pub headers: Vec<(String, String)>,
    pub users: u32,
    pub ramp_up_secs: u64,
    pub duration_secs: u64,
    pub request_limit: Option<u64>,
    pub timeout_secs: u64,
    pub on_error: OnError,
    pub auth: AuthStrategy,
    pub shared_username: String,
    /// Never written to disk.
    #[serde(skip)]
    pub shared_password: String,
    /// Run once per virtual user before its first iteration (login and/or registration).
    pub login_steps: Vec<Step>,
    pub scenarios: Vec<Scenario>,
}

impl Default for TestDefinition {
    fn default() -> Self {
        TestDefinition {
            base_url: "http://localhost:8080".into(),
            headers: Vec::new(),
            users: 10,
            ramp_up_secs: 10,
            duration_secs: 60,
            request_limit: None,
            timeout_secs: 30,
            on_error: OnError::default(),
            auth: AuthStrategy::default(),
            shared_username: String::new(),
            shared_password: String::new(),
            login_steps: Vec::new(),
            scenarios: vec![Scenario::default()],
        }
    }
}

impl TestDefinition {
    /// Returns all validation errors; empty means valid.
    pub fn validate(&self) -> Vec<String> {
        let mut errs = Vec::new();
        let base = match Url::parse(&self.base_url) {
            Ok(u) if matches!(u.scheme(), "http" | "https") => Some(u),
            _ => {
                errs.push("Base URL must be a valid http(s) URL".into());
                None
            }
        };
        if !(1..=MAX_USERS).contains(&self.users) {
            errs.push(format!("Virtual users must be 1..={MAX_USERS}"));
        }
        if !(1..=MAX_DURATION_SECS).contains(&self.duration_secs) {
            errs.push(format!("Duration must be 1..={MAX_DURATION_SECS} s"));
        }
        if self.ramp_up_secs > self.duration_secs {
            errs.push("Ramp-up must not exceed the duration".into());
        }
        if self.request_limit == Some(0) {
            errs.push("Request limit must be at least 1".into());
        }
        if !(1..=300).contains(&self.timeout_secs) {
            errs.push("Timeout must be 1..=300 s".into());
        }
        check_headers("Default headers", &self.headers, &mut errs);
        match self.auth {
            AuthStrategy::None => {}
            AuthStrategy::Shared if self.shared_username.trim().is_empty() => {
                errs.push("Shared account needs a username".into());
            }
            _ if self.login_steps.is_empty() => {
                errs.push("Authentication needs at least one login step".into());
            }
            _ => {}
        }
        for (i, s) in self.login_steps.iter().enumerate() {
            check_step(
                &format!("Login step {}", i + 1),
                s,
                base.as_ref(),
                &mut errs,
            );
        }
        if self.scenarios.is_empty() {
            errs.push("At least one scenario is required".into());
        }
        for sc in &self.scenarios {
            if sc.steps.is_empty() {
                errs.push(format!("Scenario '{}' has no steps", sc.name));
            }
            for (i, s) in sc.steps.iter().enumerate() {
                check_step(
                    &format!("'{}' step {}", sc.name, i + 1),
                    s,
                    base.as_ref(),
                    &mut errs,
                );
            }
        }
        errs
    }

    pub fn load(path: &str) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))
    }

    pub fn save(&self, path: &str) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| format!("{path}: {e}"))
    }
}

/// Reads `username,password` rows (with header) from CSV text.
pub fn parse_accounts(text: &str) -> Result<Vec<Account>, String> {
    let mut rdr = csv::Reader::from_reader(text.as_bytes());
    let headers = rdr.headers().map_err(|e| e.to_string())?.clone();
    let col = |name: &str| {
        headers
            .iter()
            .position(|h| h.trim().eq_ignore_ascii_case(name))
            .ok_or(format!("missing column '{name}'"))
    };
    let (u, p) = (col("username")?, col("password")?);
    let mut accounts = Vec::new();
    for (i, row) in rdr.records().enumerate() {
        // Never echo row contents: they contain passwords.
        let row = row.map_err(|_| format!("row {}: malformed", i + 2))?;
        let username = row.get(u).unwrap_or("").to_string();
        if username.is_empty() {
            return Err(format!("row {}: empty username", i + 2));
        }
        let password = row.get(p).unwrap_or("").to_string();
        accounts.push(Account { username, password });
    }
    Ok(accounts)
}

pub fn load_accounts(path: &str) -> Result<Vec<Account>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    parse_accounts(&text)
}

/// True for localhost, loopback and private-network addresses. This is only a hint
/// for the start confirmation; it cannot tell whether a host is a production system.
pub fn is_local_host(url: &str) -> bool {
    use std::net::IpAddr;
    let Some(host) = Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
    else {
        return false;
    };
    match host.trim_matches(['[', ']']).parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Ok(IpAddr::V6(ip)) => {
            ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local()
        }
        Err(_) => host == "localhost" || host.ends_with(".localhost"),
    }
}

/// Resolves a step URL against the base URL. Placeholders are substituted before this.
pub fn resolve_url(base: &Url, url: &str) -> Result<Url, String> {
    let u = if url.starts_with("http://") || url.starts_with("https://") {
        Url::parse(url)
    } else {
        base.join(url)
    };
    u.map_err(|e| e.to_string())
}

fn check_step(label: &str, s: &Step, base: Option<&Url>, errs: &mut Vec<String>) {
    // Placeholders are replaced at runtime; validate with a neutral stand-in.
    let url = s
        .url
        .replace("{{username}}", "u")
        .replace("{{password}}", "p")
        .replace("{{vu_id}}", "0");
    if let Some(base) = base
        && resolve_url(base, &url).is_err()
    {
        errs.push(format!("{label}: invalid URL"));
    }
    if s.pause_min_ms > s.pause_max_ms {
        errs.push(format!("{label}: pause min > max"));
    }
    if s.expected_status.iter().any(|c| !(100..=599).contains(c)) {
        errs.push(format!("{label}: expected status must be 100..=599"));
    }
    check_headers(label, &s.headers, errs);
}

fn check_headers(label: &str, headers: &[(String, String)], errs: &mut Vec<String>) {
    for (k, _) in headers {
        if reqwest::header::HeaderName::from_bytes(k.as_bytes()).is_err() {
            errs.push(format!("{label}: invalid header name '{k}'"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_valid() {
        assert_eq!(TestDefinition::default().validate(), Vec::<String>::new());
    }

    #[test]
    fn catches_invalid_fields() {
        let mut d = TestDefinition {
            base_url: "ftp://x".into(),
            users: MAX_USERS + 1,
            duration_secs: 0,
            timeout_secs: 0,
            request_limit: Some(0),
            auth: AuthStrategy::Shared,
            ..Default::default()
        };
        d.scenarios[0].steps[0].pause_min_ms = 5;
        d.scenarios[0].steps[0].expected_status = vec![42];
        d.scenarios[0].steps[0].headers = vec![("bad header".into(), "x".into())];
        let errs = d.validate();
        for needle in [
            "Base URL",
            "Virtual users",
            "Duration",
            "Ramp-up",
            "Request limit",
            "Timeout",
            "username",
            "pause",
            "status",
            "header",
        ] {
            assert!(
                errs.iter().any(|e| e.contains(needle)),
                "missing {needle}: {errs:?}"
            );
        }
    }

    #[test]
    fn auth_needs_login_steps() {
        let d = TestDefinition {
            auth: AuthStrategy::PerUser,
            ..Default::default()
        };
        assert!(d.validate().iter().any(|e| e.contains("login step")));
    }

    #[test]
    fn empty_scenario_rejected() {
        let mut d = TestDefinition::default();
        d.scenarios[0].steps.clear();
        assert!(d.validate().iter().any(|e| e.contains("no steps")));
        d.scenarios.clear();
        assert!(d.validate().iter().any(|e| e.contains("At least one")));
    }

    #[test]
    fn status_ok_defaults_to_2xx() {
        let mut s = Step::default();
        assert!(s.status_ok(204) && !s.status_ok(302));
        s.expected_status = vec![302];
        assert!(s.status_ok(302) && !s.status_ok(200));
    }

    #[test]
    fn resolves_relative_and_absolute() {
        let base = Url::parse("http://h:1/app/").unwrap();
        assert_eq!(
            resolve_url(&base, "x").unwrap().as_str(),
            "http://h:1/app/x"
        );
        assert_eq!(resolve_url(&base, "/y").unwrap().as_str(), "http://h:1/y");
        assert_eq!(
            resolve_url(&base, "https://o/z").unwrap().as_str(),
            "https://o/z"
        );
    }

    #[test]
    fn parses_accounts_csv() {
        let a = parse_accounts("username,password\nalice,\"pa,ss\"\"word\"\nbob,\n").unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].password, "pa,ss\"word");
        assert_eq!(a[1].password, "");
        let swapped = parse_accounts("Password,Username\nx,y\n").unwrap();
        assert_eq!(swapped[0].username, "y");
    }

    #[test]
    fn account_errors_do_not_leak_passwords() {
        assert!(
            parse_accounts("user,pw\na,b\n")
                .unwrap_err()
                .contains("username")
        );
        let e = parse_accounts("username,password\n,topsecret\n").unwrap_err();
        assert!(e.contains("row 2") && !e.contains("topsecret"));
        let e = parse_accounts("username,password\na,topsecret,extra\n").unwrap_err();
        assert!(!e.contains("topsecret"), "{e}");
    }

    #[test]
    fn classifies_local_hosts() {
        for u in [
            "http://localhost:8080",
            "http://127.0.0.1",
            "http://10.1.2.3",
            "http://192.168.0.5",
            "http://172.20.0.1",
            "http://[::1]/",
            "http://app.localhost",
        ] {
            assert!(is_local_host(u), "{u}");
        }
        for u in [
            "https://example.com",
            "http://8.8.8.8",
            "http://172.32.0.1",
            "not a url",
        ] {
            assert!(!is_local_host(u), "{u}");
        }
    }

    #[test]
    fn json_roundtrip_without_password() {
        let d = TestDefinition {
            auth: AuthStrategy::Shared,
            shared_username: "alice".into(),
            shared_password: "s3cret-pw".into(),
            ..Default::default()
        };
        let path = std::env::temp_dir().join(format!("powerwizz-{}.json", std::process::id()));
        let path = path.to_str().unwrap();
        d.save(path).unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        assert!(!text.contains("s3cret-pw"));
        let back = TestDefinition::load(path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(back.shared_password, "");
        assert_eq!(
            TestDefinition {
                shared_password: String::new(),
                ..d
            },
            back
        );
    }
}
