#!/usr/bin/env bash
# Throwaway MySQL 8.4 and MariaDB 11.4 in Docker, for the database tests, with TLS from a throwaway CA, and one more
# MariaDB with TLS off.
#
#   scripts/mysql.sh up     start both, wait until they accept connections, write .mysql/env
#   scripts/mysql.sh down   remove both and .mysql/
#   scripts/mysql.sh env    print the connection lines the tests read:
#                             eval "$(scripts/mysql.sh env)"
#
# Each runs as root with a random password, published on loopback only. Nothing on the host is
# installed or touched apart from the two containers this script labels as its own.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
state="$repo/.mysql"
envfile="$state/env"
# Marks the containers as ones this script made.
label="speedyspec.mysql=1"
# name|image|port|variable|tls; images pinned, so the tests mean the same thing every time.
servers=(
    "speedyspec-mysql|mysql:8.4|13306|SPEEDYSPEC_E2E_MYSQL|on"
    "speedyspec-mariadb|mariadb:11.4|13307|SPEEDYSPEC_E2E_MARIADB|on"
    "speedyspec-mariadb-plain|mariadb:11.4|13308|SPEEDYSPEC_E2E_MARIADB_NO_TLS|off"
)

# A throwaway CA and two server certificates it signs: MySQL's names 127.0.0.1, MariaDB's names only another host,
# so a check of the host name has something to refuse. And another CA, which signed nothing they present. The keys
# never leave .mysql/ and the containers.
make_certificates() {
    local dir="$state/tls"
    mkdir -p "$dir"
    chmod 700 "$dir"
    openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj "/CN=speedyspec test CA" \
        -keyout "$dir/ca-key.pem" -out "$dir/ca.pem" 2>/dev/null
    openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj "/CN=speedyspec other CA" \
        -keyout "$dir/other-ca-key.pem" -out "$dir/other-ca.pem" 2>/dev/null
    local server san
    for server in "speedyspec-mysql|IP:127.0.0.1" "speedyspec-mariadb|DNS:other-host.invalid"; do
        IFS='|' read -r server san <<< "$server"
        openssl req -newkey rsa:2048 -nodes -subj "/CN=${san#*:}" \
            -keyout "$dir/$server-key.pem" -out "$dir/$server.csr" 2>/dev/null
        printf 'subjectAltName=%s\n' "$san" > "$dir/$server.ext"
        openssl x509 -req -in "$dir/$server.csr" -CA "$dir/ca.pem" -CAkey "$dir/ca-key.pem" -CAcreateserial \
            -days 2 -extfile "$dir/$server.ext" -out "$dir/$server.pem" 2>/dev/null
        rm -f "$dir/$server.csr" "$dir/$server.ext"
    done
    rm -f "$dir/ca.srl"
    echo "export SPEEDYSPEC_E2E_MYSQL_CA=$dir/ca.pem" >> "$envfile"
    echo "export SPEEDYSPEC_E2E_OTHER_CA=$dir/other-ca.pem" >> "$envfile"
}

exists() {
    [ -n "$(docker ps -a --quiet --filter "name=^$1\$")" ]
}

mine() {
    [ -n "$(docker ps -a --quiet --filter "name=^$1\$" --filter "label=${label}")" ]
}

refuse_foreign() {
    if exists "$1" && ! mine "$1"; then
        echo "scripts/mysql.sh: a container named $1 exists and this script did not create it" >&2
        exit 1
    fi
}

case "${1:-}" in
    up)
        mkdir -p "$state"
        chmod 700 "$state"
        : > "$envfile"
        chmod 600 "$envfile"
        make_certificates
        for server in "${servers[@]}"; do
            IFS='|' read -r name image port variable tls <<< "$server"
            refuse_foreign "$name"
            if exists "$name"; then
                docker rm --force --volumes "$name" >/dev/null
            fi
            password="$(head -c 18 /dev/urandom | od -An -tx1 | tr -d ' \n')"
            case "$image" in
                mysql:*) secret_env=(--env "MYSQL_ROOT_PASSWORD=$password") ;;
                *) secret_env=(--env "MARIADB_ROOT_PASSWORD=$password") ;;
            esac
            if [ "$tls" = "on" ]; then
                server_args=(--ssl-ca=/etc/speedyspec-tls/ca.pem --ssl-cert=/etc/speedyspec-tls/server.pem
                    --ssl-key=/etc/speedyspec-tls/server-key.pem)
            else
                server_args=(--skip-ssl)
            fi
            docker create --name "$name" --label "$label" \
                --publish "127.0.0.1:${port}:3306" "${secret_env[@]}" "$image" "${server_args[@]}" >/dev/null
            if [ "$tls" = "on" ]; then
                # Copied in rather than mounted, so the server's own user can read them whatever the host's modes.
                staged="$(mktemp -d)"
                cp "$state/tls/ca.pem" "$staged/ca.pem"
                cp "$state/tls/$name.pem" "$staged/server.pem"
                cp "$state/tls/$name-key.pem" "$staged/server-key.pem"
                chmod 755 "$staged"
                chmod 644 "$staged"/*.pem
                docker cp "$staged/." "$name:/etc/speedyspec-tls" >/dev/null
                rm -rf "$staged"
            fi
            docker start "$name" >/dev/null
            echo "export ${variable}=mysql://root:${password}@127.0.0.1:${port}" >> "$envfile"
        done
        # Ready when a query over TCP succeeds (the images restart once during first setup).
        for server in "${servers[@]}"; do
            IFS='|' read -r name image port variable tls <<< "$server"
            password="$(sed -n "s#^export ${variable}=mysql://root:\(.*\)@.*#\1#p" "$envfile")"
            client=(mysql)
            [ "${image%%:*}" = "mariadb" ] && client=(mariadb)
            # MariaDB's own client insists on TLS unless told the server has none.
            [ "$tls" = "off" ] && client+=(--skip-ssl)
            ready=""
            for _ in $(seq 1 90); do
                if docker exec "$name" "${client[@]}" --protocol=TCP -h127.0.0.1 -uroot "-p$password" \
                    -e 'SELECT 1' >/dev/null 2>&1; then
                    ready=1
                    break
                fi
                sleep 2
            done
            if [ -z "$ready" ]; then
                echo "scripts/mysql.sh: $name did not become ready; its log follows" >&2
                docker logs --tail 40 "$name" >&2 || true
                exit 1
            fi
        done
        echo "$envfile"
        ;;
    down)
        for server in "${servers[@]}"; do
            IFS='|' read -r name _ _ _ _ <<< "$server"
            refuse_foreign "$name"
            if exists "$name"; then
                docker rm --force --volumes "$name" >/dev/null
            fi
        done
        rm -rf "$state"
        ;;
    env)
        if [ ! -f "$envfile" ]; then
            echo "scripts/mysql.sh: not running; run scripts/mysql.sh up first" >&2
            exit 1
        fi
        cat "$envfile"
        ;;
    *)
        echo "usage: scripts/mysql.sh up | down | env" >&2
        exit 2
        ;;
esac
