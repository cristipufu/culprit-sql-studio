use super::*;
#[cfg(any(feature = "auth-azure", feature = "auth-aws"))]
use culprit_core::AuthKind;
use serde_json::json;

#[test]
fn password_profiles_keep_named_identity_and_do_not_resolve_the_environment_during_configuration() {
    let profile: CredentialProfile = serde_json::from_value(json!({
        "kind":"password-environment", "username":"selected-user", "variable":"CULPRIT_UNUSED_ADAPTER_PROFILE"
    }))
    .unwrap();
    let profiles = ConfiguredCredentials::new(&[("selected".into(), profile)].into()).unwrap();
    profiles.validate_password("selected").unwrap();
    assert!(profiles.validate_password("another-identity").is_err());
    assert!(profiles.database("selected").is_err());
    assert!(profiles.bearer("selected").is_err());
    assert!(profiles.access_keys("selected").is_err());
}
#[test]
fn azure_profile_schema_and_construction_follow_the_compiled_capability_without_token_acquisition() {
    let profile = serde_json::from_value::<CredentialProfile>(json!({
        "kind":"azure", "identity":{"kind":"managed-identity","client_id":null}
    }));
    assert_eq!(profile.is_ok(), cfg!(feature = "auth-azure"));
    #[cfg(feature = "auth-azure")]
    {
        let profiles = ConfiguredCredentials::new(&[("selected".into(), profile.unwrap())].into()).unwrap();
        assert_eq!(profiles.database("selected").unwrap().auth_kind(), AuthKind::Entra);
        assert!(profiles.bearer("selected").is_ok());
        assert!(profiles.access_keys("selected").is_err());
        assert!(profiles.validate_password("selected").is_err());
    }
}
#[test]
fn aws_profile_schema_and_construction_follow_the_compiled_capability_without_cli_or_token_acquisition() {
    let profile = serde_json::from_value::<CredentialProfile>(json!({"kind":"aws","profile":"owned-profile"}));
    assert_eq!(profile.is_ok(), cfg!(feature = "auth-aws"));
    #[cfg(feature = "auth-aws")]
    {
        let profiles = ConfiguredCredentials::new(&[("selected".into(), profile.unwrap())].into()).unwrap();
        assert_eq!(profiles.database("selected").unwrap().auth_kind(), AuthKind::AwsIam);
        assert!(profiles.access_keys("selected").is_ok());
        assert!(profiles.bearer("selected").is_err());
        assert!(profiles.validate_password("selected").is_err());
    }
}
