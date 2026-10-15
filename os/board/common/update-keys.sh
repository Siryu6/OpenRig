# Key signing the update bundles and its certificate, which the images
# trust (/etc/rauc/keyring.pem). Sourced by post-build.sh and
# make-bundle.sh. Made on first use; keep it: images built with another
# key refuse the updates signed with this one.
#   PEDALBOARD_UPDATE_KEYS  directory of key.pem and cert.pem (os/keys)

UPDATE_KEYS=${PEDALBOARD_UPDATE_KEYS:-$(cd "$(dirname "$0")/../.." && pwd)/keys}
UPDATE_KEY="$UPDATE_KEYS/key.pem"
UPDATE_CERT="$UPDATE_KEYS/cert.pem"

if [ ! -e "$UPDATE_KEY" ]; then
	echo "Creating the update signing key in $UPDATE_KEYS: keep it safe" >&2
	mkdir -p "$UPDATE_KEYS"
	openssl=openssl
	[ ! -x "${HOST_DIR:-}/bin/openssl" ] || openssl="$HOST_DIR/bin/openssl"
	(
		umask 077
		"$openssl" req -x509 -newkey rsa:4096 -nodes -days 36500 \
			-subj "/CN=Pedalboard OS updates" \
			-keyout "$UPDATE_KEY" -out "$UPDATE_CERT" 2>/dev/null
	)
	chmod 0644 "$UPDATE_CERT"
fi
