#!/usr/bin/env bash
# Télécharge et décompresse les n-grammes français de LanguageTool dans ./ngrams/fr
# (plusieurs Go : prévois l'espace disque). Ils améliorent la détection des mots
# souvent confondus (a/à, ces/ses, quand/quant…).
set -euo pipefail
cd "$(dirname "$0")"

BASE="https://languagetool.org/download/ngram-data"
FILE=$(curl -fsSL "$BASE/" | grep -o 'ngrams-fr-[0-9]*\.zip' | sort -u | tail -n1 || true)
if [ -z "$FILE" ]; then
  echo "Archive des n-grammes français introuvable sur $BASE/ — vérifie la page à la main." >&2
  exit 1
fi

mkdir -p ngrams
echo "Téléchargement de $FILE (reprise possible si interrompu)…"
curl -fL --continue-at - -o "ngrams/$FILE" "$BASE/$FILE"
echo "Décompression…"
unzip -q -o "ngrams/$FILE" -d ngrams
rm -f "ngrams/$FILE"

if [ -d ngrams/fr ]; then
  echo "OK : $(du -sh ngrams/fr | cut -f1) dans $(pwd)/ngrams/fr"
  echo "Décommente « langtool_languageModel=/ngrams » dans docker-compose.yml puis : docker compose up -d"
else
  echo "Attention : le dossier ngrams/fr est absent, vérifie le contenu de l'archive." >&2
  exit 1
fi
