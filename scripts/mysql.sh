#!/usr/bin/env bash
# Throwaway MySQL 8.4 and MariaDB 11.4 in Docker, for the database tests.
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
# name|image|port|variable; images pinned, so the tests mean the same thing every time.
servers=(
    "speedyspec-mysql|mysql:8.4|13306|SPEEDYSPEC_E2E_MYSQL"
    "speedyspec-mariadb|mariadb:11.4|13307|SPEEDYSPEC_E2E_MARIADB"
)

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
        for server in "${servers[@]}"; do
            IFS='|' read -r name image port variable <<< "$server"
            refuse_foreign "$name"
            if exists "$name"; then
                docker rm --force --volumes "$name" >/dev/null
            fi
            password="$(head -c 18 /dev/urandom | od -An -tx1 | tr -d ' \n')"
            case "$image" in
                mysql:*) secret_env=(--env "MYSQL_ROOT_PASSWORD=$password") ;;
                *) secret_env=(--env "MARIADB_ROOT_PASSWORD=$password") ;;
            esac
            docker run --detach --name "$name" --label "$label" \
                --publish "127.0.0.1:${port}:3306" "${secret_env[@]}" "$image" >/dev/null
            echo "export ${variable}=mysql://root:${password}@127.0.0.1:${port}" >> "$envfile"
        done
        # Ready when a query over TCP succeeds (the images restart once during first setup).
        for server in "${servers[@]}"; do
            IFS='|' read -r name image port variable <<< "$server"
            password="$(sed -n "s#^export ${variable}=mysql://root:\(.*\)@.*#\1#p" "$envfile")"
            client=mysql
            [ "${image%%:*}" = "mariadb" ] && client=mariadb
            ready=""
            for _ in $(seq 1 90); do
                if docker exec "$name" "$client" --protocol=TCP -h127.0.0.1 -uroot "-p$password" \
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
            IFS='|' read -r name _ _ _ <<< "$server"
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
