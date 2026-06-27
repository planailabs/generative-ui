//! Direct-access demo: the server, the UI agent API, and a swiftide LLM agent all run in
//! one process as separate libraries.
//!
//! "Direct access" = everything (HTTP server + agent) lives in one binary, but each piece is
//! a separate, independently-tested crate that is simply *invoked* here. The swiftide agent
//! conducts a conversation entirely through interactive UI: it publishes a prompt surface,
//! waits for the user's submission (backflow), asks the LLM, and republishes the surface with
//! the answer — the same create → wait → respond flow the MCP server exposes, in-process.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use gui_agent::{SurfaceBuilder, Workspace};
use gui_schema::{Component, DeckManifest, SurfacePath};
use swiftide::agents::Agent;
use swiftide::chat_completion::{
    ChatCompletion, ChatCompletionRequest, ChatCompletionResponse, ChatMessage,
    errors::LanguageModelError,
};

pub const AGENT: &str = "assistant";
pub const DECK: &str = "main";
pub const SURFACE: &str = "chat";

/// A deterministic LLM stand-in for tests and offline demos: it echoes the user's last
/// message. Implements swiftide's [`ChatCompletion`] so it drops into a real `Agent`.
#[derive(Clone, Default)]
pub struct MockLlm;

#[async_trait]
impl ChatCompletion for MockLlm {
    async fn complete(
        &self,
        request: &ChatCompletionRequest<'_>,
    ) -> std::result::Result<ChatCompletionResponse, LanguageModelError> {
        let prompt = request
            .messages()
            .iter()
            .rev()
            .find_map(|m| match m {
                ChatMessage::User(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default();
        // A message with no tool calls is a final answer, so the agent stops here.
        let answer = format!("You said: {}. (mock answer)", prompt.trim());
        Ok(ChatCompletionResponse::builder()
            .message(answer)
            .build()
            .expect("response builds"))
    }
}

/// Run one LLM turn through a real swiftide `Agent` and return the assistant's text.
pub async fn answer<L>(llm: &L, prompt: &str) -> Result<String>
where
    L: ChatCompletion + Clone + 'static,
{
    let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let sink = captured.clone();

    let mut agent = Agent::builder()
        .llm(llm)
        .on_new_message(move |_agent, message| {
            // Extract synchronously (the message borrow can't cross the async boundary).
            let text = match &*message {
                ChatMessage::Assistant(Some(t), _) => Some(t.clone()),
                _ => None,
            };
            let sink = sink.clone();
            Box::pin(async move {
                if let Some(t) = text {
                    *sink.lock().unwrap() = Some(t);
                }
                Ok(())
            })
        })
        .build()?;

    agent.query(prompt.to_string()).await?;
    let out = captured.lock().unwrap().take().unwrap_or_default();
    Ok(out)
}

/// Build the chat surface — an optional last answer above a prompt form.
fn chat_surface(last_answer: Option<&str>) -> SurfaceBuilder {
    let mut b = SurfaceBuilder::new("Assistant", "A prompt box to talk to the assistant")
        .heading("Assistant");
    if let Some(ans) = last_answer {
        b = b.component(Component::Card {
            children: vec![Component::Text { text: ans.to_string() }],
        });
    }
    b.component(Component::Form {
        action: "ask".into(),
        children: vec![Component::Input {
            name: "prompt".into(),
            label: Some("Your message".into()),
            placeholder: Some("Ask me anything…".into()),
            input_type: None,
        }],
    })
    .action("ask")
}

/// Run the interactive session loop: publish the chat surface, wait for the user to submit a
/// prompt (backflow), ask the LLM, and republish with the answer. Loops forever.
pub async fn run_session<L>(ws: Workspace, llm: L) -> Result<()>
where
    L: ChatCompletion + Clone + 'static,
{
    let path = SurfacePath::new(AGENT, DECK, SURFACE)?;
    ws.define_deck(
        AGENT,
        DECK,
        &DeckManifest {
            name: Some("Assistant".into()),
            entry: Some(SURFACE.into()),
            order: vec![SURFACE.into()],
        },
    )?;

    let mut last: Option<String> = None;
    loop {
        ws.publish(&path, &chat_surface(last.as_deref()))?;

        // The user's submission arrives as backflow. The wait is blocking (filesystem
        // watch), so run it off the async executor.
        let ws2 = ws.clone();
        let p2 = path.clone();
        let event =
            tokio::task::spawn_blocking(move || ws2.wait_for_backflow(&p2, None, Duration::from_secs(3600)))
                .await??;
        let Some(event) = event else { continue };

        let prompt = event
            .event
            .value
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if prompt.trim().is_empty() {
            continue;
        }

        let reply = answer(&llm, &prompt)
            .await
            .unwrap_or_else(|e| format!("Sorry — something went wrong: {e}"));
        last = Some(reply);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_llm_answers_through_a_real_agent() {
        let reply = answer(&MockLlm, "hello there").await.unwrap();
        assert!(reply.contains("hello there"), "got: {reply}");
        assert!(reply.contains("mock answer"));
    }
}
