#!/bin/bash

# Vérifier si le script est exécuté avec les droits root
if [ "$EUID" -ne 0 ]; then
  echo "❌ Veuillez exécuter ce script avec sudo : sudo ./uninstall_service.sh"
  exit 1
fi

SERVICE_FILE="/etc/systemd/system/xfiles-bot.service"

echo "🛑 Arrêt et désinstallation du service systemd du bot X-Files..."

# 1. Arrêt du service
if systemctl is-active --quiet xfiles-bot.service; then
  echo "🛑 Arrêt du service en cours..."
  systemctl stop xfiles-bot.service
else
  echo "ℹ️ Le service n'est pas actif actuellement."
fi

# 2. Désactivation au démarrage
if systemctl is-enabled --quiet xfiles-bot.service 2>/dev/null; then
  echo "➖ Désactivation du service au démarrage..."
  systemctl disable xfiles-bot.service
fi

# 3. Suppression du fichier de service
if [ -f "$SERVICE_FILE" ]; then
  echo "🗑️ Suppression du fichier de service..."
  rm "$SERVICE_FILE"
fi

# 4. Rechargement de systemd
echo "🔄 Rechargement de systemd..."
systemctl daemon-reload

echo "✅ Désinstallation terminée avec succès. Le bot a été arrêté et le service retiré."
