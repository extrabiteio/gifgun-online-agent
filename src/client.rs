use reqwest::{Client, Method};
use serde_json::Value;

use crate::error::{AgentError, AgentResult};
use crate::session::AgentSessionRecord;

#[derive(Clone)]
pub struct AgentClient {
    client: Client,
    session: AgentSessionRecord,
}

impl AgentClient {
    pub fn new(session: AgentSessionRecord) -> AgentResult<Self> {
        let client = Client::builder()
            .no_proxy()
            .build()
            .map_err(|_| AgentError::Bridge("the local HTTP client is unavailable".into()))?;
        Ok(Self { client, session })
    }

    pub fn session(&self) -> &AgentSessionRecord {
        &self.session
    }

    pub async fn get(&self, path: &str) -> AgentResult<Value> {
        self.request(Method::GET, path, None).await
    }

    pub async fn post(&self, path: &str, body: Value) -> AgentResult<Value> {
        self.request(Method::POST, path, Some(body)).await
    }

    async fn request(&self, method: Method, path: &str, body: Option<Value>) -> AgentResult<Value> {
        let url = format!("http://127.0.0.1:{}{path}", self.session.port);
        let mut request = self
            .client
            .request(method, url)
            .bearer_auth(self.session.agent_token.expose());
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|_| AgentError::Bridge("the local bridge is unavailable".into()))?;
        let status = response.status();
        let value = response
            .json::<Value>()
            .await
            .map_err(|_| AgentError::Bridge("the local bridge returned invalid JSON".into()))?;
        if status.is_success() {
            return Ok(value);
        }
        let message = value
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| value.get("error").and_then(Value::as_str))
            .unwrap_or("the local bridge rejected the request");
        Err(AgentError::Bridge(message.to_owned()))
    }
}
