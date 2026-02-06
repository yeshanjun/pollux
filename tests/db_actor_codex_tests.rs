use pollux::db::{CodexCreate, CodexPatch, ProviderCreate, ProviderPatch};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::SystemTime;
use tokio::fs;

#[tokio::test]
async fn test_codex_db_actor_baseline() {
    let tmp_dir = std::env::temp_dir();
    let mut hasher = DefaultHasher::new();
    SystemTime::now().hash(&mut hasher);
    let db_file_name = format!("test_codex_db_{}.sqlite", hasher.finish());
    let db_path = tmp_dir.join(db_file_name);
    let database_url = format!("sqlite:{}", db_path.to_str().unwrap());

    // Spawn DbActor
    let db_actor_handle = pollux::db::spawn(&database_url).await;

    // 1. Assert list_active_codex() returns Ok(empty) on a fresh DB
    let active_codex_keys = db_actor_handle.list_active_codex().await.unwrap();
    assert!(
        active_codex_keys.is_empty(),
        "Expected no active Codex keys initially"
    );

    // 2. Create a Codex account/token row
    let email = Some("test@example.com".to_string());
    let account_id = "acct-test-id".to_string();
    let sub = "auth0|test-subject".to_string();
    let refresh_token = "rt-test-token".to_string();
    let access_token = "at-test-token".to_string();
    let expiry = chrono::Utc::now();
    let chatgpt_plan_type = Some("plus".to_string());

    let create_data = CodexCreate {
        email: email.clone(),
        account_id: account_id.clone(),
        sub: sub.clone(),
        refresh_token: refresh_token.clone(),
        access_token: access_token.clone(),
        expiry,
        chatgpt_plan_type: chatgpt_plan_type.clone(),
    };
    let provider_create = ProviderCreate::Codex(create_data);

    let id = db_actor_handle.create(provider_create).await.unwrap();
    assert!(id > 0, "Expected a valid ID after creation");

    // 3. Assert list_active_codex() returns 1 row with matching fields and status true
    let active_codex_keys_after_create = db_actor_handle.list_active_codex().await.unwrap();
    assert_eq!(
        active_codex_keys_after_create.len(),
        1,
        "Expected one active Codex row after creation"
    );

    let codex_key = active_codex_keys_after_create.first().unwrap();
    assert_eq!(codex_key.id, id);
    assert_eq!(codex_key.email, email);
    assert_eq!(codex_key.account_id, account_id);
    assert_eq!(codex_key.sub, sub);
    assert_eq!(codex_key.refresh_token, refresh_token);
    assert_eq!(codex_key.access_token, access_token);
    assert_eq!(codex_key.expiry, expiry);
    assert_eq!(codex_key.chatgpt_plan_type, chatgpt_plan_type);
    assert!(codex_key.status);

    // 4. Assert get_codex_by_id() returns the same row
    let fetched_codex_key = db_actor_handle.get_codex_by_id(id).await.unwrap();
    assert_eq!(fetched_codex_key, *codex_key);

    // 5. Patch status to false
    let patch_data = CodexPatch {
        status: Some(false),
        ..Default::default()
    };
    let provider_patch = ProviderPatch::Codex {
        id: u64::try_from(id).unwrap(),
        patch: patch_data,
    };
    db_actor_handle.patch(provider_patch).await.unwrap();

    // 6. Assert list_active_codex() returns empty after disable
    let active_codex_keys_after_patch = db_actor_handle.list_active_codex().await.unwrap();
    assert!(
        active_codex_keys_after_patch.is_empty(),
        "Expected no active Codex keys after disabling"
    );

    // Clean up the temporary database file
    let wal_path = std::path::PathBuf::from(format!("{}-wal", db_path.to_string_lossy()));
    let shm_path = std::path::PathBuf::from(format!("{}-shm", db_path.to_string_lossy()));
    let _ = fs::remove_file(&wal_path).await;
    let _ = fs::remove_file(&shm_path).await;
    fs::remove_file(&db_path).await.unwrap();
}
