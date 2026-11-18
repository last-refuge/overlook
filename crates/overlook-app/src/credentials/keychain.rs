//! Generic-password items in the user's macOS Keychain. Use the default local
//! keychain so a source build can store items without signing entitlements.

use super::APPLICATION;
use overlook_core::AccountId;
use security_framework::base::Result;
use security_framework::item::{ItemClass, ItemSearchOptions, Limit};
use security_framework::passwords::{
    delete_generic_password, generic_password, set_generic_password, PasswordOptions,
};
use security_framework_sys::base::errSecItemNotFound;

/// The service namespaces the account so sign-out can delete every kind with one query.
fn service(account: &AccountId) -> String {
    format!("{APPLICATION}:{}", account.0)
}

pub(super) fn save(account: &AccountId, kind: &str, secret: Vec<u8>) -> Result<()> {
    set_generic_password(&service(account), kind, &secret)
}

pub(super) fn load(account: &AccountId, kind: &str) -> Result<Option<Vec<u8>>> {
    optional(generic_password(PasswordOptions::new_generic_password(
        &service(account),
        kind,
    )))
}

pub(super) fn delete(account: &AccountId, kind: &str) -> Result<()> {
    optional(delete_generic_password(&service(account), kind)).map(|_| ())
}

pub(super) fn delete_all(account: &AccountId) -> Result<()> {
    let mut query = ItemSearchOptions::new();
    // The file-based macOS Keychain otherwise deletes only the first match.
    query
        .class(ItemClass::generic_password())
        .service(&service(account))
        .limit(Limit::All);
    optional(query.delete()).map(|_| ())
}

fn optional<T>(result: Result<T>) -> Result<Option<T>> {
    match result {
        Err(error) if error.code() == errSecItemNotFound => Ok(None),
        result => result.map(Some),
    }
}

#[cfg(test)]
mod tests {
    use crate::credentials::{
        delete_all_secrets, delete_secret, load_microsoft_tokens, load_password,
        save_microsoft_tokens, save_password, KIND_PASSWORD_OUTGOING,
    };
    use overlook_core::AccountId;
    use overlook_provider::microsoft::ClientConfig;
    use overlook_provider::oauth::OAuthTokens;

    struct TestAccount(AccountId);

    impl Drop for TestAccount {
        fn drop(&mut self) {
            let _ = delete_all_secrets(&self.0);
        }
    }

    #[test]
    #[ignore = "uses disposable synthetic entries in the local macOS Keychain"]
    fn macos_keychain_round_trip() {
        let unique = tempfile::tempdir().unwrap();
        let suffix = unique.path().file_name().unwrap().to_string_lossy();
        let test_account = TestAccount(AccountId(format!("overlook-test:{suffix}")));
        let other_account = TestAccount(AccountId(format!("overlook-test:{suffix}:other")));
        let account = &test_account.0;
        let other = &other_account.0;
        let calendar_kind = "password:calendar:synthetic";
        let config = ClientConfig::new("synthetic-client", "consumers").unwrap();
        let mut tokens = OAuthTokens {
            access_token: "synthetic-access".into(),
            refresh_token: Some("synthetic-refresh".into()),
            expires_at: 5,
            scope: None,
            signed_in_at: 7,
        };

        assert!(load_microsoft_tokens(account).unwrap().is_none());
        save_microsoft_tokens(account, &tokens, &config).unwrap();
        assert_eq!(
            load_microsoft_tokens(account).unwrap(),
            Some((tokens.clone(), config.clone()))
        );
        tokens.refresh_token = Some("synthetic-refreshed".into());
        save_microsoft_tokens(account, &tokens, &config).unwrap();
        assert_eq!(
            load_microsoft_tokens(account).unwrap(),
            Some((tokens, config))
        );

        save_password(account, KIND_PASSWORD_OUTGOING, "synthetic-smtp").unwrap();
        save_password(account, calendar_kind, "synthetic-calendar").unwrap();
        save_password(other, calendar_kind, "synthetic-other").unwrap();
        delete_secret(account, KIND_PASSWORD_OUTGOING).unwrap();
        assert!(load_password(account, KIND_PASSWORD_OUTGOING)
            .unwrap()
            .is_none());
        assert!(load_microsoft_tokens(account).unwrap().is_some());

        delete_all_secrets(account).unwrap();
        assert!(load_microsoft_tokens(account).unwrap().is_none());
        assert!(load_password(account, calendar_kind).unwrap().is_none());
        assert_eq!(
            load_password(other, calendar_kind).unwrap().as_deref(),
            Some("synthetic-other")
        );
        delete_all_secrets(other).unwrap();
    }
}
