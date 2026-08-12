use serde_json::json;

use crate::common::{ApiResult, MessageContent, STORY_TEMPERATURE};

const URL: &str = "https://api.mistral.ai/v1/chat/completions";
const MODEL: &str = "mistral-large-latest";
const SUMMARY_MODEL: &str = "mistral-small-latest";

async fn call(
    client: &reqwest::Client,
    api_key: &str,
    payload: serde_json::Value,
) -> ApiResult<String> {
    let response = client
        .post(URL)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&payload)
        .send()
        .await?;

    if !response.status().is_success() {
        let error_text = response.text().await?;
        return Err(format!("Mistral API returned error: {}", error_text).into());
    }

    let response_json: serde_json::Value = response.json().await?;
    let text = response_json["choices"][0]["message"]["content"]
        .as_str()
        .ok_or("Failed to extract content from Mistral response")?
        .to_string();

    Ok(text)
}

// Génère la suite de l'histoire ; retourne le JSON brut (schéma StoryResponse).
pub async fn complete_story(
    client: &reqwest::Client,
    api_key: &str,
    system_text: &str,
    history: &[MessageContent],
) -> ApiResult<String> {
    let mut messages = vec![json!({
        "role": "system",
        "content": system_text
    })];

    // Historique récent (le rôle 'model' de l'état partagé devient 'assistant' pour Mistral)
    for turn in history {
        let role = match turn.role.as_str() {
            "model" | "assistant" => "assistant",
            _ => "user",
        };
        let content: String = turn
            .parts
            .iter()
            .map(|p| p.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        messages.push(json!({
            "role": role,
            "content": content
        }));
    }

    let payload = json!({
        "model": MODEL,
        "messages": messages,
        "response_format": {
            "type": "json_object"
        },
        "temperature": STORY_TEMPERATURE
    });

    call(client, api_key, payload).await
}

pub async fn complete_turn_plan(
    client: &reqwest::Client,
    api_key: &str,
    system_text: &str,
    history: &[MessageContent],
) -> ApiResult<String> {
    let mut messages = vec![json!({ "role": "system", "content": system_text })];
    for turn in history {
        let role = match turn.role.as_str() {
            "model" | "assistant" => "assistant",
            _ => "user",
        };
        let content = turn
            .parts
            .iter()
            .map(|part| part.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        messages.push(json!({ "role": role, "content": content }));
    }

    let payload = json!({
        "model": SUMMARY_MODEL,
        "messages": messages,
        "response_format": { "type": "json_object" },
        "temperature": 0.1
    });
    call(client, api_key, payload).await
}

// Complétion texte simple (résumés).
pub async fn complete_text(
    client: &reqwest::Client,
    api_key: &str,
    system_text: &str,
    prompt: &str,
    temperature: f32,
) -> ApiResult<String> {
    let payload = json!({
        "model": SUMMARY_MODEL,
        "messages": [
            { "role": "system", "content": system_text },
            { "role": "user", "content": prompt }
        ],
        "response_format": { "type": "json_object" },
        "temperature": temperature
    });

    call(client, api_key, payload).await
}
