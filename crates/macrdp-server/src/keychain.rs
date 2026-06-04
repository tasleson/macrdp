use anyhow::Context;

const SERVICE: &str = "macrdp";

fn account(username: Option<&str>) -> String {
    username
        .map(str::to_owned)
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "default".to_string())
}

pub fn set_password(username: Option<&str>, password: &str) -> anyhow::Result<()> {
    let acct = account(username);
    keyring::Entry::new(SERVICE, &acct)
        .context("Failed to create keychain entry")?
        .set_password(password)
        .context("Failed to store password in keychain")?;
    Ok(())
}

pub fn get_password(username: Option<&str>) -> anyhow::Result<String> {
    let acct = account(username);
    keyring::Entry::new(SERVICE, &acct)
        .context("Failed to create keychain entry")?
        .get_password()
        .context("Failed to read password from keychain; run with --keychain-set-password to store one")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_uses_provided_username() {
        assert_eq!(account(Some("alice")), "alice");
    }

    #[test]
    fn account_falls_back_to_env_user() {
        // Just verify it returns something non-empty when no username given.
        let acct = account(None);
        assert!(!acct.is_empty());
    }
}
