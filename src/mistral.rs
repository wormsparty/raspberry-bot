#![allow(dead_code)]

use serde_json::json;
use std::error::Error;

use crate::common::{
    ConversationState, MessageContent, Part, StoryResponse, MAX_RECENT_TURNS, SUMMARY_TRIGGER,
    SYSTEM_INSTRUCTION,
};

async fn summarize_history(
    api_key: &str,
    summary_so_far: &str,
    turns_to_summarize: &[MessageContent],
) -> Result<String, Box<dyn Error + Send + Sync>> {
    let client = reqwest::Client::new();
    let url = "https://api.mistral.ai/v1/chat/completions";

    let history_text: String = turns_to_summarize
        .iter()
        .map(|m| format!("[{}]: {}", m.role, m.parts.iter().map(|p| p.text.as_str()).collect::<Vec<_>>().join(" ")))
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = if summary_so_far.is_empty() {
        format!(
            "Voici des échanges d'un jeu de rôle X-Files. Résume en 4-5 phrases : où sont Mulder et Scully, quels indices ont été découverts, où en est l'enquête.\n\n{}",
            history_text
        )
    } else {
        format!(
            "Résumé existant de l'enquête :\n{}\n\nNouveaux échanges à intégrer :\n{}\n\nProduis un résumé mis à jour en 4-5 phrases maximum.",
            summary_so_far, history_text
        )
    };

    let request_payload = json!({
        "model": "mistral-small-latest",
        "messages": [
            { "role": "user", "content": prompt }
        ],
        "temperature": 0.3
    });

    let response = client.post(url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&request_payload)
        .send()
        .await?;

    if !response.status().is_success() {
        let error_text = response.text().await?;
        return Err(format!("Mistral summarize error: {}", error_text).into());
    }

    let response_json: serde_json::Value = response.json().await?;
    let text = response_json["choices"][0]["message"]["content"]
        .as_str()
        .ok_or("Failed to extract summary text from Mistral response")?
        .to_string();

    Ok(text)
}

pub async fn generate_story(
    api_key: &str,
    state: &mut ConversationState,
    new_user_message: &str,
) -> Result<StoryResponse, Box<dyn Error + Send + Sync>> {
    // 1. Ajouter le message utilisateur
    state.recent.push(MessageContent {
        role: "user".to_string(),
        parts: vec![Part { text: new_user_message.to_string() }],
    });

    // 2. Si trop de tours, résumer les anciens et ne garder que les récents
    if state.recent.len() > SUMMARY_TRIGGER {
        let split_at = state.recent.len() - MAX_RECENT_TURNS / 2;
        let to_summarize = state.recent[..split_at].to_vec();
        let kept = state.recent[split_at..].to_vec();

        state.summary = summarize_history(api_key, &state.summary, &to_summarize).await?;
        state.recent = kept;
    }

    // 3. Construire le contexte : consigne système + résumé + historique récent
    let mut messages = Vec::new();

    // Injecter la consigne système (avec le résumé de l'enquête s'il existe)
    let system_text = if state.summary.is_empty() {
        format!(
            "{}\n\nIMPORTANT: Tu dois impérativement répondre sous la forme d'un objet JSON contenant la clé suivante :\n- \"story_text\" (string) : Le texte décrivant la suite de l'histoire et les actions des personnages",
            SYSTEM_INSTRUCTION
        )
    } else {
        format!(
            "{}\n\n[Résumé de l'enquête jusqu'ici : {}]\n\nIMPORTANT: Tu dois impérativement répondre sous la forme d'un objet JSON contenant la clé suivante :\n- \"story_text\" (string) : Le texte décrivant la suite de l'histoire et les actions des personnages",
            SYSTEM_INSTRUCTION, state.summary
        )
    };

    messages.push(json!({
        "role": "system",
        "content": system_text
    }));

    // Ajouter l'historique récent (en faisant correspondre le rôle 'model' à 'assistant' pour Mistral)
    for turn in &state.recent {
        let role = match turn.role.as_str() {
            "model" | "assistant" => "assistant",
            _ => "user",
        };
        let content: String = turn.parts.iter().map(|p| p.text.as_str()).collect::<Vec<_>>().join(" ");
        messages.push(json!({
            "role": role,
            "content": content
        }));
    }

    // 4. Appel API Mistral avec format JSON
    let client = reqwest::Client::new();
    let url = "https://api.mistral.ai/v1/chat/completions";

    let request_payload = json!({
        "model": "mistral-large-latest",
        "messages": messages,
        "response_format": {
            "type": "json_object"
        },
        "temperature": 0.7
    });

    let response = client.post(url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&request_payload)
        .send()
        .await?;

    if !response.status().is_success() {
        let error_text = response.text().await?;
        return Err(format!("Mistral API returned error: {}", error_text).into());
    }

    let response_json: serde_json::Value = response.json().await?;
    let text = response_json["choices"][0]["message"]["content"]
        .as_str()
        .ok_or("Failed to extract content from Mistral choice")?;

    let story_response: StoryResponse = match serde_json::from_str(text) {
        Ok(res) => res,
        Err(e) => {
            log::error!("Failed to parse StoryResponse from Mistral. Raw text was: {}", text);
            return Err(e.into());
        }
    };

    // 5. Sauvegarder la réponse du modèle dans l'historique récent (avec le rôle "model" pour compatibilité)
    state.recent.push(MessageContent {
        role: "model".to_string(),
        parts: vec![Part { text: story_response.story_text.clone() }],
    });

    Ok(story_response)
}

pub async fn get_story_summary(
    api_key: &str,
    state: &ConversationState,
) -> Result<String, Box<dyn Error + Send + Sync>> {
    if state.recent.is_empty() {
        if state.summary.is_empty() {
            return Ok("Aucune enquête n'est en cours.".to_string());
        } else {
            return Ok(state.summary.clone());
        }
    }
    summarize_history(api_key, &state.summary, &state.recent).await
}

