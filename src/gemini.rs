use serde::{Deserialize, Serialize};
use serde_json::json;
use std::error::Error;
//use base64::{engine::general_purpose, Engine as _};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MessageContent {
    pub role: String,
    pub parts: Vec<Part>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Part {
    pub text: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StoryResponse {
    pub story_text: String,
    pub should_generate_image: bool,
    pub image_prompt: String,
}

// Nouvelle structure pour gérer l'état de la conversation
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ConversationState {
    pub summary: String,
    pub recent: Vec<MessageContent>,
}

const MAX_RECENT_TURNS: usize = 6; // 3 échanges user/model
const SUMMARY_TRIGGER: usize = MAX_RECENT_TURNS; // résumé quand on dépasse

const SYSTEM_INSTRUCTION: &str = r#"
Tu es le Maître de Jeu d'un jeu de rôle sérieux et immersif, dans l'esprit de la série télévisée X-Files.
Les joueurs suivent les aventures de Fox Mulder et Dana Scully, agents spéciaux du FBI.

Directives de ton et de style :
1. Le ton est sérieux, professionnel et tendu — exactement comme dans la série. Mulder et Scully traitent chaque affaire avec le plus grand sérieux du FBI. L'humour naît du décalage entre ce sérieux et la situation objective (un canard qui cite Kant, une maison qui marche comme un crabe) — mais les personnages, eux, ne trouvent pas ça drôle.
2. Les mystères peuvent prendre n'importe quelle forme — phénomènes paranormaux, créatures insolites, anomalies physiques, comportements inexplicables — mais ils doivent être présentés comme de vraies enquêtes avec des témoins, des indices, des pistes. Le phénomène bizarre existe, il est juste traité avec le protocole FBI standard.
3. Respecte scrupuleusement la dynamique Mulder/Scully : Mulder est convaincu d'emblée que c'est paranormal et cherche à le prouver avec un enthousiasme sincère. Scully cherche l'explication rationnelle avec la même sincérité. Aucun des deux ne fait de l'humour volontairement. C'est leur sérieux absolu face à l'incongruité objective qui crée le comique.
4. Reste concis : 1 à 3 paragraphes maximum par réponse. Pas de gras, pas de listes. Prose narrative, style téléfilm.
5. L'enquête doit progresser à chaque tour : nouveaux indices, rebondissements, suspects, lieux. Évite les actions sans conséquence. Chaque message fait avancer l'histoire.
6. Termine toujours par une situation ouverte ou une observation qui invite le joueur à décrire son action suivante.

Directives de génération d'image :
1. Illustre les moments clés : découverte d'un indice, apparition du phénomène, confrontation.
2. Ne mentionne pas 'Mulder', 'Scully' ou 'X-Files' dans le prompt d'image. Décris-les : 'a male FBI agent in a dark suit' et 'a female FBI agent with red bob hair and a trench coat'.
3. Style visuel : 'A grainy, retro 1990s television sci-fi series VHS screenshot of [scène]. Muted colors, dark moody lighting, foggy atmosphere, 35mm film grain, analog video distortion.'
"#;

async fn summarize_history(
    api_key: &str,
    summary_so_far: &str,
    turns_to_summarize: &[MessageContent],
) -> Result<String, Box<dyn Error + Send + Sync>> {
    let client = reqwest::Client::new();
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash-lite:generateContent?key={}",
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

// Nouvelle version qui prend un ConversationState au lieu de &[MessageContent]
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
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash-lite:generateContent?key={}",
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
                    },
                    "should_generate_image": {
                        "type": "boolean",
                        "description": "Vrai si l'action ou la scène actuelle mérite grandement une illustration visuelle"
                    },
                    "image_prompt": {
                        "type": "string",
                        "description": "Le prompt en anglais décrivant précisément l'illustration de style X-files à générer, seulement si should_generate_image est vrai. Sinon, chaîne vide."
                    }
                },
                "required": ["story_text", "should_generate_image", "image_prompt"]
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
