# Serveur LanguageTool auto-hébergé (Proxmox + Tailscale)

Le serveur public `https://api.languagetool.org` suffit pour tester, mais il limite le nombre de requêtes par minute et la taille des textes, et tes textes partent chez un tiers. Ton propre serveur supprime ces limites, garde tes textes chez toi et sert aussi l'extension navigateur LanguageTool.

## 1. Conteneur LXC sur Proxmox

Les valeurs suivantes conviennent pour un usage personnel :

| Ressource | Sans n-grammes | Avec n-grammes FR |
|-----------|----------------|-------------------|
| vCPU      | 2              | 2                 |
| RAM       | 2 Go           | 3 Go              |
| Disque    | 8 Go           | 20 Go             |

Utilise un modèle Debian 12. Docker dans un LXC nécessite l'option **nesting** :

```bash
pct set <CTID> --features nesting=1,keyctl=1
```

Dans le conteneur :

```bash
apt update && apt install -y curl unzip
curl -fsSL https://get.docker.com | sh
```

## 2. Lancement

Copie le dossier `server/` dans le conteneur, puis :

```bash
cd server
docker compose up -d
curl -s -d "language=fr&text=Bonjoru les amis est là" http://localhost:8010/v2/check | head -c 300
```

Le premier démarrage prend environ 30 secondes.

## 3. N-grammes français (facultatif, recommandé)

Les n-grammes permettent de repérer les confusions de mots (a/à, ses/ces, quand/quant…). Le téléchargement pèse plusieurs Go :

```bash
./get-ngrams-fr.sh
# puis décommente « langtool_languageModel=/ngrams » dans docker-compose.yml
docker compose up -d
```

## 4. Accès depuis tes PC via Tailscale

Installe Tailscale dans le LXC (ou expose le port depuis l'hôte), puis choisis une des deux options.

**Option simple (HTTP sur le tailnet)** : dans Coquille, indique `http://<ip-tailscale-du-lxc>:8010`, ou le nom MagicDNS du conteneur.

**Option HTTPS avec certificat valide** : à préférer pour l'extension navigateur.

```bash
tailscale serve --bg 8010
# → https://<machine>.<tailnet>.ts.net
```

Dans Coquille, va dans ⚙ › Serveur LanguageTool, colle l'URL, puis clique sur **Tester**.

> **Cloudflared** : n'expose pas ce serveur publiquement avec un tunnel sans protection. LanguageTool n'a pas d'authentification. Si tu le protèges avec Cloudflare Access, Coquille ne pourra pas s'y connecter, car il n'envoie pas de jeton Access. Tailscale est la bonne solution ici.

## 5. Bonus : correction en direct dans le navigateur

L'extension officielle LanguageTool (Firefox, Chrome, Edge) peut utiliser **ton** serveur, ce qui donne la correction en direct dans Gmail, les formulaires, etc. :

1. Ouvre les paramètres de l'extension, puis **Paramètres avancés** › **Serveur LanguageTool**.
2. Choisis **Autre serveur** et saisis l'URL de ton serveur suivie de `/v2`, par exemple `https://lt.<tailnet>.ts.net/v2`.

Si Firefox refuse la connexion (problème CORS connu), consulte le [contournement fourni avec l'image Docker](https://github.com/Erikvl87/docker-languagetool/tree/main/cors-workaround).

LibreOffice sait aussi utiliser un serveur LanguageTool distant : Outils › Options › Paramètres linguistiques › Serveur distant LanguageTool.

## Ajouter des mots au dictionnaire du serveur

Le dictionnaire personnel de Coquille est local à chaque PC. Pour un dictionnaire commun à tous tes appareils (y compris l'extension), suis la section « Improving the spell checker » du [README de l'image](https://github.com/Erikvl87/docker-languagetool#improving-the-spell-checker). Pour le français, le fichier est `org/languagetool/resource/fr/hunspell/spelling.txt`.
