#[cfg(test)]
pub mod fixtures {
    use base64::Engine;
    use serde::Serialize;
    use serde_json::Map;
    use serde_json::Value;
    use serde_json::json;
    use std::future::Future;
    use tokio::time::Duration;
    use tokio::time::timeout;

    fn b64url_no_pad(bytes: &[u8]) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    pub fn make_test_jwt(email: Option<&str>, plan: Option<&str>) -> String {
        #[derive(Serialize)]
        struct Header {
            alg: &'static str,
            typ: &'static str,
        }
        let header = Header {
            alg: "none",
            typ: "JWT",
        };

        let mut payload_map = Map::new();
        if let Some(email) = email {
            payload_map.insert("email".to_string(), json!(email));
        }
        if let Some(plan) = plan {
            payload_map.insert(
                "https://api.openai.com/auth".to_string(),
                Value::Object(
                    [("chatgpt_plan_type".to_string(), json!(plan))]
                        .into_iter()
                        .collect(),
                ),
            );
        }
        let payload = Value::Object(payload_map);

        let header_b64 = b64url_no_pad(&serde_json::to_vec(&header).unwrap());
        let payload_b64 = b64url_no_pad(&serde_json::to_vec(&payload).unwrap());
        let signature_b64 = b64url_no_pad(b"sig");
        format!("{header_b64}.{payload_b64}.{signature_b64}")
    }

    pub async fn await_with_timeout<F, T>(duration: Duration, future: F, context: &str) -> T
    where
        F: Future<Output = T>,
    {
        timeout(duration, future)
            .await
            .unwrap_or_else(|_| panic!("{context} timed out after {duration:?}"))
    }
}
