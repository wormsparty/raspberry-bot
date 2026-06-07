use serde::{Deserialize, Serialize};
use serde_json::json;
use std::error::Error;
use base64::{engine::general_purpose, Engine as _};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MessageContent {
    pub role: String, // "user" or "model"
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

const SYSTEM_INSTRUCTION: &str = r#"
Tu es le Maître de Jeu d'un jeu de rôle textuel absurde et humoristique (style AI Dungeon).
Les joueurs suivent les aventures de Fox Mulder et Dana Scully, les célèbres agents du FBI.

Directives de ton et de style :
1. Le ton doit être drôle, absurde, décalé et mystérieux. L'humour doit être présent à chaque tour.
2. Les mystères ne doivent PAS être limités aux extraterrestres. L'absurdité peut prendre n'importe quelle forme :
   - Des maisons qui sortent de terre et se mettent à marcher comme des crabes.
   - Un tableau de Sigmund Freud qui apparaît de façon récurrente et inexpliquée dans des endroits improbables (dans un tiroir, sur un arbre, sous le chapeau d'un suspect).
   - Des animaux avec des comportements philosophiques, des objets du quotidien dotés de parole, des lois de la physique temporairement suspendues.
3. Conserve la dynamique de Mulder (le croyant obsessionnel, prêt à croire aux théories les plus folles) et Scully (la scientifique sceptique qui tente de trouver une explication rationnelle même face à une maison qui marche).
4. Reste concis : écris 1 à 3 paragraphes maximum par réponse pour que ce soit agréable à lire sur Telegram.
5. Termine toujours par une situation ouverte ou une question qui invite le joueur à décrire son action suivante.

Directives de génération d'image :
1. Tu dois évaluer si la scène actuelle mérite d'être illustrée visuellement (par exemple, si une maison marche, si le tableau de Freud apparaît, si Mulder fait une découverte insolite).
2. Si une illustration est pertinente, définis `should_generate_image` à true et rédige un prompt en anglais dans `image_prompt`.
3. Pour contourner les filtres de sécurité sur les visages de célébrités ou marques déposées, ne mentionne PAS directement 'Mulder' ou 'Scully' ou 'X-Files' dans le prompt d'image. Décris-les plutôt ainsi : 'A male FBI agent in a dark 90s suit and a female FBI agent with red bob hair and a trench coat'.
4. Formate le prompt d'image pour imiter le rendu visuel de la série télévisée des années 90 :
   'A grainy, retro 1990s television sci-fi series VHS screenshot of [description détaillée de la scène]. Muted colors, dark moody lighting, foggy atmosphere, 35mm film grain, analog video distortion.'
"#;

pub async fn generate_story(
    api_key: &str,
    history: &[MessageContent],
) -> Result<StoryResponse, Box<dyn Error + Send + Sync>> {
    let client = reqwest::Client::new();
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent?key={}",
        api_key
    );

    let request_payload = json!({
        "contents": history,
        "systemInstruction": {
            "parts": [
                {
                    "text": SYSTEM_INSTRUCTION
                }
            ]
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

    let response = client
        .post(&url)
        .json(&request_payload)
        .send()
        .await?;

    if !response.status().is_success() {
        let error_text = response.text().await?;
        return Err(format!("Gemini API returned error: {}", error_text).into());
    }

    let response_json: serde_json::Value = response.json().await?;
    
    let text = response_json["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .ok_or("Failed to extract text from Gemini response")?;

    let story_response: StoryResponse = serde_json::from_str(text)?;
    Ok(story_response)
}

pub async fn generate_image(
    api_key: &str,
    prompt: &str,
) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
    let client = reqwest::Client::new();
    let model = std::env::var("IMAGEN_MODEL").unwrap_or_else(|_| "imagen-4.0-generate-001".to_string());
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:predict?key={}",
        model, api_key
    );

    let request_payload = json!({
        "instances": [
            {
                "prompt": prompt
            }
        ],
        "parameters": {
            "sampleCount": 1,
            "aspectRatio": "16:9",
            "outputMimeType": "image/jpeg"
        }
    });

    let response = client
        .post(&url)
        .json(&request_payload)
        .send()
        .await?;

    if !response.status().is_success() {
        let error_text = response.text().await?;
        return Err(format!("Imagen API returned error: {}", error_text).into());
    }

    let response_json: serde_json::Value = response.json().await?;
    
    let base64_data = response_json["predictions"][0]["bytesBase64Encoded"]
        .as_str()
        .ok_or("Failed to extract image base64 data from Imagen response")?;

    // The base64 crate is used to decode the image bytes
    let image_bytes = general_purpose::STANDARD.decode(base64_data)?;
    Ok(image_bytes)
}
