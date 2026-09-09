use super::*;

#[test]
fn key_private_clears_with_empty_string() {
    let vault = unlocked_vault();
    let key = SshKey::new("my-key", KeyAlgorithm::Ed25519);
    vault.save_key(&key, Some("-----BEGIN PRIVATE KEY-----\nx\n-----END PRIVATE KEY-----")).unwrap();
    assert!(vault.get_key_private(&key.id).unwrap().is_some());

    // `Some("")` clears the private key (NULL), not an encrypted empty blob.
    vault.save_key(&key, Some("")).unwrap();
    assert_eq!(vault.get_key_private(&key.id).unwrap(), None);
}


#[test]
fn security_key_row_roundtrips_with_null_private() {
    // B3 structural test: a security-key (public-only) row is persisted
    // with an explicit NULL private column and lists back with its sk-
    // algorithm intact. `import_public_key` has no private input path by
    // construction; this pins the storage half of that invariant.
    let vault = unlocked_vault();
    let mut key = SshKey::new("yubi", KeyAlgorithm::SkEd25519);
    key.public_key = "sk-ssh-ed25519@openssh.com AAAA... user@example.com".into();
    key.fingerprint = "SHA256:stub".into();
    vault.save_key(&key, Some("")).unwrap();

    let listed = vault.list_keys().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].algorithm, KeyAlgorithm::SkEd25519);
    assert!(listed[0].algorithm.is_security_key());
    // The public-only row reports no private material (drives the combo
    // filter + agent-server exclusion, B3 review F2/F3).
    assert!(!listed[0].has_private);
    assert_eq!(vault.get_key_private(&key.id).unwrap(), None);

    // The other sk- family maps through the same string pair.
    let mut ec = SshKey::new("yubi-ec", KeyAlgorithm::SkEcdsaP256);
    ec.public_key = "sk-ecdsa-sha2-nistp256@openssh.com AAAA...".into();
    vault.save_key(&ec, Some("")).unwrap();
    let listed = vault.list_keys().unwrap();
    let ec_row = listed.iter().find(|k| k.label == "yubi-ec").unwrap();
    assert_eq!(ec_row.algorithm, KeyAlgorithm::SkEcdsaP256);
}


#[test]
fn save_and_list_keys() {
    let vault = unlocked_vault();
    let key = SshKey::new("my-key", KeyAlgorithm::Ed25519);
    vault.save_key(&key, Some("-----BEGIN PRIVATE KEY-----\nfake\n-----END PRIVATE KEY-----")).unwrap();

    let keys = vault.list_keys().unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].label, "my-key");
    // A row saved with real private material reports has_private = true.
    assert!(keys[0].has_private);
}


#[test]
fn key_private_encrypted_and_retrievable() {
    let vault = unlocked_vault();
    let key = SshKey::new("test-key", KeyAlgorithm::Rsa4096);
    let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIE...\n-----END RSA PRIVATE KEY-----";
    vault.save_key(&key, Some(pem)).unwrap();

    let retrieved = vault.get_key_private(&key.id).unwrap();
    assert_eq!(retrieved, Some(pem.to_string()));
}


#[test]
fn delete_key() {
    let vault = unlocked_vault();
    let key = SshKey::new("disposable", KeyAlgorithm::Ed25519);
    vault.save_key(&key, None).unwrap();
    vault.delete_key(&key.id).unwrap();
    assert_eq!(vault.list_keys().unwrap().len(), 0);
}

// ── Groups CRUD ──


#[test]
fn certificate_persists_through_save_and_list() {
    let vault = unlocked_vault();
    let mut key = SshKey::new("cert-key", KeyAlgorithm::Ed25519);
    let cert = "ssh-ed25519-cert-v01@openssh.com AAAAtest... user@ca";
    key.certificate = Some(cert.to_string());
    vault.save_key(&key, Some("-----BEGIN PRIVATE KEY-----\nx\n-----END PRIVATE KEY-----")).unwrap();

    let keys = vault.list_keys().unwrap();
    assert_eq!(keys[0].certificate.as_deref(), Some(cert));

    // Clearing it (None field, plain overwrite) drops the cert.
    key.certificate = None;
    vault.save_key(&key, None).unwrap();
    assert_eq!(vault.list_keys().unwrap()[0].certificate, None);
}


#[test]
fn key_has_updated_at() {
    let vault = unlocked_vault();
    let key = SshKey::new("test-key", KeyAlgorithm::Ed25519);
    assert!(key.updated_at.timestamp() > 0);
    vault.save_key(&key, None).unwrap();

    let keys = vault.list_keys().unwrap();
    assert_eq!(keys.len(), 1);
    assert!(keys[0].updated_at.timestamp() > 0);
}


/// The re-offer pass stamps only the rows that have something to send.
/// A public-only row keeps its stamp, which is what keeps last-writer-
/// wins pointing at the device that holds the material rather than at
/// the one that received an empty row from an older peer.
#[test]
fn touch_only_stamps_keys_that_hold_private_material() {
    let vault = unlocked_vault();

    let with_private = SshKey::new("has-material", KeyAlgorithm::Ed25519);
    vault
        .save_key(&with_private, Some("-----BEGIN PRIVATE KEY-----\nx\n"))
        .unwrap();

    let public_only = SshKey::new("public-only", KeyAlgorithm::SkEd25519);
    vault.save_key(&public_only, None).unwrap();

    let before: std::collections::HashMap<_, _> = vault
        .list_keys()
        .unwrap()
        .into_iter()
        .map(|k| (k.id, k.updated_at))
        .collect();

    // The stamp is second-resolution on the wire, so make the change
    // observable rather than racing the same instant.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert_eq!(vault.touch_keys_with_private_material().unwrap(), 1);

    let after: std::collections::HashMap<_, _> = vault
        .list_keys()
        .unwrap()
        .into_iter()
        .map(|k| (k.id, k.updated_at))
        .collect();
    assert!(
        after[&with_private.id] > before[&with_private.id],
        "a key holding private material must be re-offered"
    );
    assert_eq!(
        after[&public_only.id], before[&public_only.id],
        "a row with no material must not out-rank the device that has it"
    );
    // The material itself is untouched by the stamp.
    assert!(vault.get_key_private(&with_private.id).unwrap().is_some());
}
