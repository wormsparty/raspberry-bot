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
    let text = match response_json["candidates"][0]["content"]["parts"][0]["text"].as_str() {
        Some(t) => t.to_string(),
        None => {
            let body = serde_json::to_string(&response_json)
                .unwrap_or_else(|_| format!("{:?}", response_json));
            return Err(format!(
                "Réponse Gemini inattendue (filtrage sécurité ou quota ?) : {}",
                body
            )
            .into());
        }
    };

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
                    "options": {
                        "type": "array",
                        "description": "Les 3 suites possibles proposées au joueur, sans numérotation, 76 caractères maximum chacune",
                        "items": { "type": "string" }
                    }
                },
                "required": ["story_text", "options"]
            },
            "temperature": STORY_TEMPERATURE
        }
    });

    call(client, api_key, payload).await
}

pub async fn complete_turn_plan(
    client: &reqwest::Client,
    api_key: &str,
    system_text: &str,
    history: &[MessageContent],
) -> ApiResult<String> {
    let payload = json!({
        "contents": history,
        "systemInstruction": { "parts": [{ "text": system_text }] },
        "generationConfig": {
            "responseMimeType": "application/json",
            "responseSchema": {
                "type": "object",
                "properties": {
                    "action_allowed": { "type": "boolean" },
                    "refusal_reason": { "type": "string" },
                    "requires_roll": { "type": "boolean" },
                    "modifiers": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "enum": ["specialty", "ally", "wounded", "improvised"]
                        }
                    },
                    "story_text": { "type": "string" },
                    "options": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                },
                "required": [
                    "action_allowed",
                    "refusal_reason",
                    "requires_roll",
                    "modifiers",
                    "story_text",
                    "options"
                ]
            },
            "temperature": 0.1
        }
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
        "contents": [{ "role": "user", "parts": [{ "text": prompt }] }],
        "systemInstruction": { "parts": [{ "text": system_text }] },
        "generationConfig": { "responseMimeType": "application/json", "temperature": temperature }
    });

    call(client, api_key, payload).await
}
