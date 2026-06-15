use base64::Engine;
use serde_json::json;

use crate::common::ApiResult;

const CHAT_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const MODEL: &str = "google/gemini-3.1-flash-image-preview";

pub async fn generate_scene_image(
    client: &reqwest::Client,
    api_key: &str,
    scene_prompt: &str,
) -> ApiResult<Vec<u8>> {
    let payload = json!({
        "model": MODEL,
        "messages": [
            {
                "role": "user",
                "content": scene_prompt
            }
        ],
        "modalities": ["text", "image"]
    });

    let response = client
        .post(CHAT_URL)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&payload)
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let error_text = response.text().await.unwrap_or_default();
        return Err(format!("OpenRouter {} : {}", status, error_text).into());
    }

    let resp: serde_json::Value = response.json().await?;

    let message = &resp["choices"][0]["message"];

    // Format OpenRouter image generation : message.images[]
    if let Some(images) = message["images"].as_array() {
        for img in images {
            if let Some(url) = img["image_url"]["url"].as_str() {
                return extract_image_bytes(client, url).await;
            }
        }
    }

    // Fallback : content tableau de parts (autres modèles multimodaux)
    let content = &message["content"];
    if let Some(parts) = content.as_array() {
        for part in parts {
            if let Some(url) = part["image_url"]["url"].as_str() {
                return extract_image_bytes(client, url).await;
            }
            if let Some(b64) = part["inlineData"]["data"].as_str() {
                return Ok(base64::engine::general_purpose::STANDARD.decode(b64)?);
            }
        }
    }

    // Fallback : content est une data URI directement dans la chaîne
    if let Some(text) = content.as_str() {
        if text.starts_with("data:image/") {
            return extract_image_bytes(client, text).await;
        }
    }

    let msg_debug = serde_json::to_string_pretty(message).unwrap_or_else(|_| format!("{:?}", message));
    Err(format!("Format de réponse image inattendu. Message: {}", msg_debug).into())
}

async fn extract_image_bytes(client: &reqwest::Client, url: &str) -> ApiResult<Vec<u8>> {
    // Data URI inline
    if let Some(stripped) = url.strip_prefix("data:image/") {
        let b64_part = stripped
            .splitn(2, ',')
            .nth(1)
            .ok_or("Data URI malformée : payload base64 absent")?;
        return Ok(base64::engine::general_purpose::STANDARD.decode(b64_part)?);
    }
    // Sécurité SSRF : on n'accepte que les URLs HTTPS
    if !url.starts_with("https://") {
        return Err(format!("URL image rejetée (schéma non autorisé) : {}", url).into());
    }
    let bytes = client.get(url).send().await?.bytes().await?;
    Ok(bytes.to_vec())
}
