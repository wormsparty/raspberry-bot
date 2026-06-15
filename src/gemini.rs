use serde_json::json;

use crate::common::{ApiResult, MessageContent, STORY_TEMPERATURE};

const MODEL: &str = "gemini-3.1-flash-lite";

fn url() -> String {
    format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
        MODEL
    )
}

async fn call(
    client: &reqwest::Client,
    api_key: &str,
    payload: serde_json::Value,
) -> ApiResult<String> {
    // Clé passée en header (et non en query param) pour éviter qu'elle
    // n'apparaisse dans les URLs des messages d'erreur reqwest.
    let response = client
        .post(url())
        .header("x-goog-api-key", api_key)
        .json(&payload)
        .send()
        .await?;

    if !response.status().is_success() {
        let error_text = response.text().await?;
        return Err(format!("Gemini API returned error: {}", error_text).into());
    }

    let response_json: serde_json::Value = response.json().await?;
    let text = response_json["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .ok_or("Failed to extract text from Gemini response")?
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
    let payload = json!({
        "contents": history,
        "systemInstruction": {
            "parts": [{ "text": system_text }]
        },
        "generationConfig": {
            "responseMimeType": "application/json",
            "responseSchema": {
                "type": "object",
                "properties": {
                    "story_text": {
                        "type": "string",
                        "description": "Le texte décrivant la suite de l'histoire et les actions des personnages"
                    },
                    "scene_description": {
                        "type": "string",
                        "description": "English image generation prompt for visually striking scenes only; empty string otherwise"
                    }
                },
                "required": ["story_text", "scene_description"]
            },
            "temperature": STORY_TEMPERATURE
        }
    });

    call(client, api_key, payload).await
}

// Complétion texte simple (résumés).
pub async fn complete_text(
    client: &reqwest::Client,
    api_key: &str,
    prompt: &str,
    temperature: f32,
) -> ApiResult<String> {
    let payload = json!({
        "contents": [{ "role": "user", "parts": [{ "text": prompt }] }],
        "generationConfig": { "temperature": temperature }
    });

    call(client, api_key, payload).await
}
