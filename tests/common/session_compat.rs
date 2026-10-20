use super::*;

#[test]
fn canonical_and_legacy_history_roundtrip_without_losing_other_clients_fields() {
    let canonical:serde_json::Value=serde_json::from_str(include_str!("../fixtures/shared-session.json")).unwrap();
    for legacy in [false,true] {
        let mut value=canonical.clone();
        if legacy {value["history"]=serde_json::Value::Array(value["history"].as_array().unwrap().iter().map(|item|{
            let mut map=serde_json::Map::new();map.insert(item["type"].as_str().unwrap().into(),item["payload"].clone());serde_json::Value::Object(map)
        }).collect());}
        let session:Session=serde_json::from_value(value).unwrap();
        assert!(session.has_conversation());assert_eq!(session.title(),"Название общей сессии");
        assert_eq!(session.history.len(),6);
        let wire=serde_json::to_value(&session).unwrap();
        assert_eq!(wire["history"][0]["type"],"UserPrompt");
        assert_eq!(wire["history"][2]["payload"]["answers"][0]["custom_text"],"С шифрованием");
        assert_eq!(wire["history"][5]["type"],"ResponseStats");
        assert_eq!(wire["token_usage"],canonical["token_usage"]);assert_eq!(wire["future_state"],canonical["future_state"]);
        assert_eq!(wire["started"],true);assert_eq!(wire["context_tokens"],42);
    }
}

#[test]
fn history_lists_hide_empty_and_invalid_files_and_keep_history_only_conversations() {
    let root=std::env::temp_dir().join(format!("takiza-compatible-sessions-{}-{}",std::process::id(),chrono::Local::now().timestamp_nanos_opt().unwrap()));
    let folder=Session::sessions_dir(&root);std::fs::create_dir_all(&folder).unwrap();
    let mut empty=serde_json::json!({"id":"empty","created_at":"","model":"fixture","started":true,"messages":[{"role":"system","content":"System instructions"}],"history":[{"type":"ResponseStats","payload":{"elapsed_ms":0,"tokens":null,"usage_complete":false}}]});
    std::fs::write(folder.join("empty.json"),empty.to_string()).unwrap();
    std::fs::write(folder.join("broken.json"),"{incomplete").unwrap();
    empty["id"]=serde_json::json!("history-only");empty["history"]=serde_json::json!([{"UserPrompt":"!echo hello"}]);
    std::fs::write(folder.join("history-only.json"),empty.to_string()).unwrap();
    let session:Session=serde_json::from_str(include_str!("../fixtures/shared-session.json")).unwrap();session.save(&root).unwrap();
    let mut ids=Session::list(&root);ids.sort();assert_eq!(ids,["history-only","shared-chat"]);
    assert_eq!(Session::load(&root,"history-only").unwrap().title(),"!echo hello");
    assert!(Session::load(&root,"shared-chat").is_some());
    assert!(folder.join("empty.json").exists());assert!(folder.join("broken.json").exists());
    std::fs::remove_dir_all(root).unwrap();
}
