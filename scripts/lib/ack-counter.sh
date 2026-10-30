#!/usr/bin/env bash
# One writer per counter. Publish in the same directory so concurrent readers
# observe either the previous complete count or the next complete count.
syntra_publish_ack() {
    local destination="$1" count="$2" staged="$1.next"
    if [[ ! "$count" =~ ^(0|[1-9][0-9]*)$ ]]; then
        printf '%s\n' 'invalid acknowledgement count' >&2
        return 2
    fi
    printf '%s\n' "$count" >"$staged" || return
    mv -f "$staged" "$destination"
}
