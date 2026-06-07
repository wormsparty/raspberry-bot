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
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-3.1-flash-lite:generateContent?key={}",
        api_key
    );

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
        "contents": [{ "role": "user", "parts": [{ "text": prompt }] }],
        "generationConfig": { "temperature": 0.3 }
    });

    let response = client.post(&url).json(&request_payload).send().await?;

    if !response.status().is_success() {
        let error_text = response.text().await?;
        return Err(format!("Gemini summarize error: {}", error_text).into());
    }

    let response_json: serde_json::Value = response.json().await?;
    let text = response_json["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .ok_or("Failed to extract summary text")?
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

    // 3. Construire le contexte : résumé injecté dans systemInstruction + tours récents
    let context = state.recent.clone();

    let system_text = if state.summary.is_empty() {
        SYSTEM_INSTRUCTION.to_string()
    } else {
        format!(
            "{}\n\n[Résumé de l'enquête jusqu'ici : {}]",
            SYSTEM_INSTRUCTION, state.summary
        )
    };

    // 4. Appel API
    let client = reqwest::Client::new();
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-3.1-flash-lite:generateContent?key={}",
        api_key
    );

    let request_payload = json!({
        "contents": context,
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
                    }
                },
                "required": ["story_text"]
            },
            "temperature": 1.0
        }
    });

    let response = client.post(&url).json(&request_payload).send().await?;

    if !response.status().is_success() {
        let error_text = response.text().await?;
        return Err(format!("Gemini API returned error: {}", error_text).into());
    }

    let response_json: serde_json::Value = response.json().await?;
    let text = response_json["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .ok_or("Failed to extract text from Gemini response")?;

    let story_response: StoryResponse = match serde_json::from_str(text) {
        Ok(res) => res,
        Err(e) => {
            log::error!("Failed to parse StoryResponse. Raw text was: {}", text);
            return Err(e.into());
        }
    };

    // 5. Sauvegarder la réponse du modèle dans l'historique récent
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

