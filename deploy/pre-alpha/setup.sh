#!/bin/sh
# First-time setup of the hosted pre-alpha hub on the host (README.md). Run from this directory as root.
# Idempotent: existing secrets are kept.
set -eu
umask 077
mkdir -p secrets
[ -f secrets/oidc-client-secret ] || openssl rand -base64 32 | tr -d '\n=+/' > secrets/oidc-client-secret
[ -f secrets/sample-password ] || printf 'prealpha-%s\n' "$(openssl rand -hex 4)" > secrets/sample-password
[ -f ../../../owner-login.txt ] || printf 'Ogun pre-alpha hub owner (Dex)\nemail:    owner@ogun.local\npassword: %s\n' "$(openssl rand -base64 18 | tr -d '\n=+/')" > ../../../owner-login.txt
hash() { docker run --rm --network none httpd:2.4-alpine htpasswd -bnBC 10 "" "$1" | tr -d ':\n' | sed 's/^[$]2y/$2a/'; }
OWNER_PW=$(sed -n 's/^password: *//p' ../../../owner-login.txt)
SAMPLE_PW=$(cat secrets/sample-password)
printf "OGUN_CLIENT_SECRET='%s'\nOWNER_PASSWORD_HASH='%s'\nSAMPLE_PASSWORD_HASH='%s'\n" \
  "$(cat secrets/oidc-client-secret)" "$(hash "$OWNER_PW")" "$(hash "$SAMPLE_PW")" > secrets/dex.env
touch secrets/agent-token secrets/desktop-token
# ogun-server and the service run as uid 10001 and read their files; Dex reads dex.env through compose.
chown 10001:10001 secrets/oidc-client-secret secrets/agent-token secrets/sample-password secrets/desktop-token
chmod 0400 secrets/oidc-client-secret
chmod 0600 secrets/agent-token secrets/desktop-token secrets/dex.env
chmod 0444 secrets/sample-password
python3 fixture/generate.py
echo "secrets ready (values not shown); sample files generated"
