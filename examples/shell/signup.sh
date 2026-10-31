#!/bin/sh
# Testing an email from a shell script: curl sends it over SMTP, `spoolbox wait` waits for it.
#
#   SPOOLBOX=./target/release/spoolbox sh examples/shell/signup.sh
#
# Set SPOOLBOX_SMTP if the server does not listen on 127.0.0.1:1025.
set -eu
spoolbox=${SPOOLBOX:-spoolbox}
smtp=${SPOOLBOX_SMTP:-127.0.0.1:1025}

mail=$(mktemp)
trap 'rm -f "$mail"' EXIT
printf 'From: Acme <noreply@acme.test>\r\nTo: dave+shell@example.com\r\nSubject: Your login link\r\n\r\nhttps://acme.test/login?once=q1w2e3\r\n' > "$mail"

# Start waiting first, then send; `wait` prints the message ID.
"$spoolbox" wait "to:dave+shell@example.com subject:login" --timeout 10s > "$mail.id" &
waiter=$!
sleep 1
curl --silent --show-error --url "smtp://$smtp" \
  --mail-from noreply@acme.test --mail-rcpt dave+shell@example.com --upload-file "$mail"
wait "$waiter"
id=$(cat "$mail.id")
rm -f "$mail.id"
echo "received $id"
