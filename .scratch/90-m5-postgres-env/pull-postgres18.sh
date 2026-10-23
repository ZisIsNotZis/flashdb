#!/usr/bin/env bash
# The postgres:18 image, as a repeatable attempt rather than a remembered intention.
#
#   bash .scratch/90-m5-postgres-env/pull-postgres18.sh            # foreground; one attempt
#   nohup bash .scratch/90-m5-postgres-env/pull-postgres18.sh &    # background; same command
#
# Every attempt resumes: the daemon keeps the layers it already holds, so a second run continues
# where the first stopped instead of starting from zero. The log is append-only and carries the
# moment of each attempt. Bandwidth only; nothing is installed on the host.
#
# After it lands (docker image inspect postgres:18 succeeds):
#   M5_PG_IMAGE=postgres:18 M5_PG_REQUIRE=<the version `select version()` reports> \
#     bash .scratch/90-m5-postgres-env/postgres-server.sh up
# and then re-run the M5 postgres arm; the version read back decides what the comparison may say.
set -uo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
log=$here/logs/pull-postgres18-$(date +%Y%m%d).out
{
    echo "# attempt $(date -Is) image=postgres:18 mirrors=$(docker info -f '{{json .RegistryConfig.Mirrors}}' 2>/dev/null)"
} >>"$log"
docker pull postgres:18 >>"$log" 2>&1
rc=$?
echo "# result: rc=$rc at $(date -Is)" >>"$log"
exit "$rc"
