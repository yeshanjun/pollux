use chrono::{Duration, Utc};
use pollux::db::{GeminiCliCreate, GeminiCliPatch, ProviderCreate, ProviderPatch};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::SystemTime;
use tokio::fs;

#[tokio::test]
async fn test_geminicli_db_actor_baseline() {
    let tmp_dir = std::env::temp_dir();
    let mut hasher = DefaultHasher::new();
    SystemTime::now().hash(&mut hasher);
    let db_file_name = format!("test_db_{}.sqlite", hasher.finish());
    let db_path = tmp_dir.join(db_file_name);
    let database_url = format!("sqlite:{}", db_path.to_str().unwrap());

    // Spawn DbActor
    let db_actor_handle = pollux::db::spawn(&database_url).await;

    // 1. Assert list_active_geminicli() returns Ok(empty) on a fresh DB
    let active_geminicli = db_actor_handle.list_active_geminicli().await.unwrap();
    assert!(
        active_geminicli.is_empty(),
        "Expected no active GeminiCli credentials initially"
    );

    // 2. Call create(ProviderCreate::GeminiCli(...))
    let project_id = "test_project_id_1".to_string();
    let sub = "google-subject-1".to_string();
    let refresh_token = "test_refresh_token_1".to_string();
    let email = Some("test@example.com".to_string());
    let access_token = Some("test_access_token_1".to_string());
    let expiry = Utc::now() + Duration::hours(1);

    let create_data = GeminiCliCreate {
        email: email.clone(),
        project_id: project_id.clone(),
        sub: sub.clone(),
        refresh_token: refresh_token.clone(),
        access_token: access_token.clone(),
        expiry,
    };
    let provider_create = ProviderCreate::GeminiCli(create_data);

    let id = db_actor_handle.create(provider_create).await.unwrap();
    assert!(id > 0, "Expected a valid ID after creation");

    // 3. Assert list_active_geminicli() returns exactly 1 row with expected fields
    let active_geminicli_after_create = db_actor_handle.list_active_geminicli().await.unwrap();
    assert_eq!(
        active_geminicli_after_create.len(),
        1,
        "Expected one active GeminiCli credential"
    );

    let credential = active_geminicli_after_create.first().unwrap();
    assert_eq!(credential.id, id);
    assert_eq!(credential.project_id, project_id);
    assert_eq!(credential.sub, sub);
    assert_eq!(credential.refresh_token, refresh_token);
    assert_eq!(credential.email, email);
    assert_eq!(credential.access_token, access_token);
    assert_eq!(credential.expiry.timestamp(), expiry.timestamp()); // Compare timestamps for equality
    assert!(credential.status);

    // 4. Patch access_token while status remains active
    let new_token = "new_token".to_string();
    let patch = GeminiCliPatch {
        access_token: Some(new_token.clone()),
        ..Default::default()
    };
    db_actor_handle
        .patch(ProviderPatch::GeminiCli {
            id: u64::try_from(id).unwrap(),
            patch,
        })
        .await
        .unwrap();

    // Verify it changed and is still in list_active_geminicli()
    let active = db_actor_handle.list_active_geminicli().await.unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].access_token, Some(new_token));

    // 5. Patch status=false
    let patch_inactive = GeminiCliPatch {
        status: Some(false),
        ..Default::default()
    };
    db_actor_handle
        .patch(ProviderPatch::GeminiCli {
            id: u64::try_from(id).unwrap(),
            patch: patch_inactive,
        })
        .await
        .unwrap();

    // Verify list_active_geminicli() is now empty
    let active_none = db_actor_handle.list_active_geminicli().await.unwrap();
    assert!(
        active_none.is_empty(),
        "Expected no active GeminiCli credentials after patching status=false"
    );

    // Clean up the temporary database file
    let wal_path = std::path::PathBuf::from(format!("{}-wal", db_path.to_string_lossy()));
    let shm_path = std::path::PathBuf::from(format!("{}-shm", db_path.to_string_lossy()));
    let _ = fs::remove_file(&wal_path).await;
    let _ = fs::remove_file(&shm_path).await;
    fs::remove_file(&db_path).await.unwrap();
}
