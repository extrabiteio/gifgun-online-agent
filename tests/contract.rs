use gifgun_agent::contract::{Compatibility, NativeContract};
use serde::Deserialize;
use serde_json::Value;

const FIXTURES: &str = include_str!("../contract/conformance-fixtures.json");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureSet {
    contract_digest: String,
    cases: Vec<Fixture>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Target {
    Pairing,
    Command,
    Result,
    CapabilityInput,
    Compatibility,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    name: String,
    target: Target,
    capability_id: Option<String>,
    accepted: bool,
    value: Value,
}

#[test]
fn embedded_contract_matches_every_browser_fixture() {
    let contract = NativeContract::load_embedded().expect("embedded contract should load");
    let fixtures: FixtureSet = serde_json::from_str(FIXTURES).expect("fixtures should parse");

    assert_eq!(fixtures.contract_digest, contract.digest());
    for fixture in fixtures.cases {
        let accepted = match fixture.target {
            Target::Pairing => contract.validate_pairing(&fixture.value).is_ok(),
            Target::Command => contract.validate_command(&fixture.value).is_ok(),
            Target::Result => contract.validate_result(&fixture.value).is_ok(),
            Target::CapabilityInput => contract
                .validate_capability_input(
                    fixture
                        .capability_id
                        .as_deref()
                        .expect("capability fixture id"),
                    &fixture.value,
                )
                .is_ok(),
            Target::Compatibility => serde_json::from_value::<Compatibility>(fixture.value.clone())
                .map(|value| contract.check_compatibility(&value).is_ok())
                .unwrap_or(false),
        };
        assert_eq!(accepted, fixture.accepted, "fixture: {}", fixture.name);
    }
}

#[test]
fn rejects_corrupt_or_network_referencing_manifests() {
    let invalid_json = NativeContract::from_json("not-json").unwrap_err();
    assert!(invalid_json.to_string().contains("manifest"));

    let external_reference = r#"{
      "formatVersion": 1,
      "protocol": {"major": 1, "minor": 0},
      "contractDigest": "digest",
      "transport": {
        "pairingEnvelope": {"$ref": "https://example.com/pairing.json"},
        "commandEnvelope": {},
        "commandResultEnvelope": {}
      },
      "capabilities": []
    }"#;
    let error = NativeContract::from_json(external_reference).unwrap_err();
    assert!(error.to_string().contains("reference"));
}

#[test]
fn bounds_validation_details_and_distinguishes_compatibility_failures() {
    let contract = NativeContract::load_embedded().unwrap();
    let error = contract
        .validate_capability_input(
            "editor.set_mode",
            &serde_json::json!({"mode": "expert", "one": 1, "two": 2}),
        )
        .unwrap_err();
    assert!(!error.issues.is_empty());
    assert!(error.issues.len() <= 8);

    let mut stale = contract.compatibility();
    stale.contract_digest = "stale-contract".to_owned();
    assert_eq!(
        contract.check_compatibility(&stale).unwrap_err().kind(),
        "contract_mismatch"
    );

    let mut protocol = contract.compatibility();
    protocol.protocol.major += 1;
    assert_eq!(
        contract.check_compatibility(&protocol).unwrap_err().kind(),
        "protocol_mismatch"
    );
}
