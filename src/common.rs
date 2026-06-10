use serde::{Deserialize, Serialize};
use std::error::Error;

pub type ApiResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ModelProvider {
    Gemini,
    Mistral,
}

impl std::fmt::Display for ModelProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelProvider::Gemini => write!(f, "Gemini"),
            ModelProvider::Mistral => write!(f, "Mistral"),
        }
    }
}

#[derive(Clone)]
pub struct Config {
    pub client: reqwest::Client,
    pub default_provider: ModelProvider,
    pub gemini_key: Option<String>,
    pub mistral_key: Option<String>,
}

impl Config {
    pub fn key_for(&self, provider: ModelProvider) -> ApiResult<&str> {
        let key = match provider {
            ModelProvider::Gemini => self.gemini_key.as_deref(),
            ModelProvider::Mistral => self.mistral_key.as_deref(),
        };
        key.ok_or_else(|| {
            format!(
                "Aucune clé API configurée pour {} (définissez {} dans l'environnement ou le fichier .env)",
                provider,
                match provider {
                    ModelProvider::Gemini => "GEMINI_API_KEY",
                    ModelProvider::Mistral => "MISTRAL_API_KEY",
                }
            )
            .into()
        })
    }
}

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
}

// Structure pour gérer l'état de la conversation.
// `provider` est sérialisé avec le reste de l'état du jeu : un changement via
// /gemini ou /mistral survit donc à un redémarrage du service.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ConversationState {
    #[serde(default)]
    pub provider: Option<ModelProvider>,
    pub summary: String,
    pub recent: Vec<MessageContent>,
}

impl ConversationState {
    pub fn provider_or(&self, default: ModelProvider) -> ModelProvider {
        self.provider.unwrap_or(default)
    }
}

pub const MAX_RECENT_TURNS: usize = 6; // 3 échanges user/model
pub const SUMMARY_TRIGGER: usize = MAX_RECENT_TURNS; // résumé quand on dépasse

pub const STORY_TEMPERATURE: f32 = 0.9;
pub const SUMMARY_TEMPERATURE: f32 = 0.3;

pub const SYSTEM_INSTRUCTION: &str = r#"
Tu es le Maître de Jeu d'un jeu de rôle sérieux et immersif, dans l'esprit de la série télévisée X-Files.
Les joueurs suivent les aventures de Fox Mulder et Dana Scully, agents spéciaux du FBI.

Directives de ton et de style :
1. Le ton est sérieux, professionnel et tendu — exactement comme dans la série. Mulder et Scully traitent chaque affaire avec le plus grand sérieux du FBI. L'humour naît du décalage entre ce sérieux et la situation objective (un canard qui cite Kant, une maison qui marche comme un crabe) — mais les personnages, eux, ne trouvent pas ça drôle.
2. Les mystères peuvent prendre n'importe quelle forme — phénomènes paranormaux, créatures insolites, anomalies physiques, comportements inexplicables — mais ils doivent être présentés comme de vraies enquêtes avec des témoins, des indices, des pistes. Le phénomène bizarre existe, il est juste traité avec le protocole FBI standard.
3. Respecte scrupuleusement la dynamique Mulder/Scully : Mulder est convaincu d'emblée que c'est paranormal et cherche à le prouver avec un enthousiasme sincère. Scully cherche l'explication rationnelle avec la même sincérité. Aucun des deux ne fait de l'humour volontairement. C'est leur sérieux absolu face à l'incongruité objective qui crée le comique.
4. Reste concis : 1 à 3 paragraphes maximum par réponse. Pas de gras, pas de listes. Prose narrative, style téléfilm.
5. L'enquête doit progresser à chaque tour : nouveaux indices, rebondissements, suspects, lieux. Évite les actions sans conséquence. Chaque message fait avancer l'histoire.
6. Termine toujours par une situation ouverte ou une observation qui invite le joueur à décrire son action suivante.
"#;

impl ModelProvider {
    // Génère la suite de l'histoire ; retourne le JSON brut produit par le modèle.
    async fn complete_story(
        &self,
        config: &Config,
        system_text: &str,
        history: &[MessageContent],
    ) -> ApiResult<String> {
        let key = config.key_for(*self)?;
        match self {
            ModelProvider::Gemini => {
                crate::gemini::complete_story(&config.client, key, system_text, history).await
            }
            ModelProvider::Mistral => {
                crate::mistral::complete_story(&config.client, key, system_text, history).await
            }
        }
    }

    // Complétion texte simple (utilisée pour les résumés).
    async fn complete_text(
        &self,
        config: &Config,
        prompt: &str,
        temperature: f32,
    ) -> ApiResult<String> {
        let key = config.key_for(*self)?;
        match self {
            ModelProvider::Gemini => {
                crate::gemini::complete_text(&config.client, key, prompt, temperature).await
            }
            ModelProvider::Mistral => {
                crate::mistral::complete_text(&config.client, key, prompt, temperature).await
            }
        }
    }
}

async fn summarize_history(
    config: &Config,
    provider: ModelProvider,
    summary_so_far: &str,
    turns_to_summarize: &[MessageContent],
) -> ApiResult<String> {
    let history_text: String = turns_to_summarize
        .iter()
        .map(|m| {
            format!(
                "[{}]: {}",
                m.role,
                m.parts.iter().map(|p| p.text.as_str()).collect::<Vec<_>>().join(" ")
            )
        })
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

    provider.complete_text(config, &prompt, SUMMARY_TEMPERATURE).await
}

pub async fn generate_story(
    config: &Config,
    state: &mut ConversationState,
    new_user_message: &str,
) -> ApiResult<StoryResponse> {
    let provider = state.provider_or(config.default_provider);

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

        state.summary = summarize_history(config, provider, &state.summary, &to_summarize).await?;
        state.recent = kept;
    }

    // 3. Construire la consigne système (avec le résumé de l'enquête s'il existe)
    let system_text = if state.summary.is_empty() {
        SYSTEM_INSTRUCTION.to_string()
    } else {
        format!(
            "{}\n\n[Résumé de l'enquête jusqu'ici : {}]",
            SYSTEM_INSTRUCTION, state.summary
        )
    };

    // 4. Appel API
    let raw = provider.complete_story(config, &system_text, &state.recent).await?;

    let story_response: StoryResponse = match serde_json::from_str(&raw) {
        Ok(res) => res,
        Err(e) => {
            log::error!("Failed to parse StoryResponse from {}. Raw text was: {}", provider, raw);
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

pub async fn get_story_summary(config: &Config, state: &ConversationState) -> ApiResult<String> {
    if state.recent.is_empty() {
        if state.summary.is_empty() {
            return Ok("Aucune enquête n'est en cours.".to_string());
        } else {
            return Ok(state.summary.clone());
        }
    }
    let provider = state.provider_or(config.default_provider);
    summarize_history(config, provider, &state.summary, &state.recent).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(role: &str, text: &str) -> MessageContent {
        MessageContent {
            role: role.to_string(),
            parts: vec![Part { text: text.to_string() }],
        }
    }

    #[test]
    fn old_session_format_still_deserializes() {
        // Format des sessions antérieures à l'ajout du champ `provider`
        let json = r#"{"summary":"un résumé","recent":[{"role":"user","parts":[{"text":"action"}]}]}"#;
        let state: ConversationState = serde_json::from_str(json).unwrap();
        assert_eq!(state.provider, None);
        assert_eq!(state.summary, "un résumé");
        assert_eq!(state.recent.len(), 1);
    }

    #[test]
    fn provider_roundtrips_with_state() {
        let state = ConversationState {
            provider: Some(ModelProvider::Mistral),
            summary: String::new(),
            recent: vec![turn("user", "action")],
        };
        let json = serde_json::to_string(&state).unwrap();
        let back: ConversationState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.provider, Some(ModelProvider::Mistral));
    }

    #[test]
    fn provider_falls_back_to_default() {
        let state = ConversationState::default();
        assert_eq!(state.provider_or(ModelProvider::Mistral), ModelProvider::Mistral);

        let state = ConversationState {
            provider: Some(ModelProvider::Gemini),
            ..Default::default()
        };
        assert_eq!(state.provider_or(ModelProvider::Mistral), ModelProvider::Gemini);
    }
}
