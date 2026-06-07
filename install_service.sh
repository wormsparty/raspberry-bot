#!/bin/bash

cd `dirname "$0"`

# Vérifier si le script est exécuté avec les droits root
if [ "$EUID" -ne 0 ]; then
  echo "❌ Veuillez exécuter ce script avec sudo : sudo ./install_service.sh"
  exit 1
fi

SERVICE_FILE="/etc/systemd/system/xfiles-bot.service"
BOT_DIR=$(pwd)
EXEC_PATH="$BOT_DIR/target/release/xfiles_bot"

echo "⚙️ Configuration du service systemd pour le bot X-Files..."

# 1. Création du fichier service
cat <<EOF > "$SERVICE_FILE"
[Unit]
Description=Mulder and Scully Telegram Bot
After=network.target

[Service]
Type=simple
User=mob
WorkingDirectory=$BOT_DIR
ExecStart=$EXEC_PATH
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

echo "✅ Fichier service créé dans $SERVICE_FILE"

# 2. Rechargement de systemd
echo "🔄 Rechargement de systemd..."
systemctl daemon-reload

# 3. Activation au démarrage
echo "➕ Activation du service au démarrage..."
systemctl enable xfiles-bot.service

# 4. Démarrage du service
echo "🚀 Démarrage du service..."
systemctl start xfiles-bot.service

# 5. Vérification du statut
echo "----------------------------------------"
systemctl status xfiles-bot.service --no-pager
echo "----------------------------------------"
echo "🎉 Installation terminée ! Le bot tourne en arrière-plan."
echo "👉 Pour voir les logs en direct, utilisez : sudo journalctl -u xfiles-bot.service -f"
