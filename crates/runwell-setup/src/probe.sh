#!/bin/sh
# Read-only Linux discovery. stdout is exactly one JSON document. No host files
# are created, no daemons started, and no credentials or environment dumped.
LC_ALL=C
export LC_ALL

quote() {
    printf '%s' "$1" | awk 'BEGIN { printf "\""; for (i=1;i<32;i++) code[sprintf("%c",i)]=i }
    { if (NR>1) printf "\\n"; for(i=1;i<=length($0);i++) { c=substr($0,i,1);
      if(c=="\\") printf "\\\\"; else if(c=="\"") printf "\\\"";
      else if(c in code) printf "\\u%04x",code[c]; else printf "%s",c } }
    END { printf "\"" }'
}
unknown() { printf '{"value":null,"unknown_reason":'; quote "$1"; printf '}'; }
raw() { printf '{"value":%s,"unknown_reason":null}' "$1"; }
string() { if [ -n "$1" ]; then printf '{"value":'; quote "$1"; printf ',"unknown_reason":null}'; else unknown "$2"; fi; }
number() {
    if printf '%s\n' "$1" | awk 'BEGIN {ok=0} /^[0-9]+([.][0-9]+)?$/ {ok=1} END {exit !ok}'; then raw "$1"; else unknown "$2"; fi
}
field() { printf '"%s":' "$1"; }
comma() { printf ','; }
words() {
    printf '['
    word_sep=''
    # Intentional whitespace splitting, with globbing disabled.
    for word in $1; do printf '%s' "$word_sep"; quote "$word"; word_sep=','; done
    printf ']'
}
set -f
linux=false
[ "$(uname -s 2>/dev/null)" = Linux ] && linux=true
privileged=false
if [ "$(id -u 2>/dev/null)" = 0 ]; then privileged=true
elif command -v sudo >/dev/null 2>&1 && sudo -n true >/dev/null 2>&1; then privileged=true; fi
# Only these fixed, read-only commands may use noninteractive sudo.
read_privileged() {
    if [ "$(id -u 2>/dev/null)" = 0 ]; then "$@" 2>/dev/null
    elif [ "$privileged" = true ]; then sudo -n "$@" 2>/dev/null
    else "$@" 2>/dev/null; fi
}
mem() { awk -v key="$1:" '$1==key {printf "%.0f",$2*1024}' /proc/meminfo 2>/dev/null; }
psi() {
    psi_values=$(awk -v kind="$2" '$1==kind {split($2,a,"=");split($3,b,"=");split($4,c,"=");printf "[%s,%s,%s]",a[2],b[2],c[2]}' "/proc/pressure/$1" 2>/dev/null)
    if [ -n "$psi_values" ]; then raw "$psi_values"; else unknown 'PSI metric unavailable'; fi
}
# Extract a JSON value without jq. Strings retain their JSON escapes; compound
# values are scanned with balanced delimiters. Only metadata files are read.
property() {
    awk -v key="$2" '
    {data=data $0 "\n"}
    END {
      for(i=1;i<=length(data);i++) {
        if(substr(data,i,1)!="\"") continue;
        start=i++; esc=0;
        for(;i<=length(data);i++) {c=substr(data,i,1); if(c=="\""&&!esc) break; if(c=="\\"&&!esc) esc=1; else esc=0}
        token=substr(data,start,i-start+1); j=i+1;
        while(substr(data,j,1)~/[ \t\r\n]/) j++;
        if(token!="\"" key "\"" || substr(data,j,1)!=":") continue;
        j++; while(substr(data,j,1)~/[ \t\r\n]/) j++;
        start=j; depth=0; quoted=0; esc=0;
        for(;j<=length(data);j++) {c=substr(data,j,1);
          if(quoted) {if(c=="\""&&!esc) quoted=0; if(c=="\\"&&!esc) esc=1;else esc=0;continue}
          if(c=="\"") {quoted=1;continue}
          if(c=="["||c=="{") depth++;
          if(c=="]"||c=="}") {if(depth==0) break; depth--;continue}
          if(depth==0&&(c==","||c~/[ \t\r\n]/)) break;
        }
        value=substr(data,start,j-start); if(value!="null") printf "%s",value; exit
      }
    }' "$1" 2>/dev/null
}
valid_metadata() {
    printf '%s' "$1" | awk -v type="$2" '
    function space() {while(substr(data,pos,1)~/[ \t\r\n]/ && pos<=length(data)) pos++}
    function text( c,escape,hex) {
      if(substr(data,pos++,1)!="\"") return 0;
      while(pos<=length(data)) {
        c=substr(data,pos++,1); if(c=="\"") return 1;
        if(c~/[[:cntrl:]]/) return 0;
        if(c=="\\") {escape=substr(data,pos++,1);
          if(escape=="u") {hex=substr(data,pos,4);if(length(hex)!=4||hex~/[^0-9a-fA-F]/) return 0;pos+=4}
          else if(escape!~/^["\\\/bfnrt]$/) return 0;
        }
      }
      return 0;
    }
    {data=data $0 "\n"}
    END {
      pos=1;space();ok=0;
      if(type=="string") ok=text();
      else if(type=="labels" && substr(data,pos++,1)=="[") {
        space();ok=1;
        if(substr(data,pos,1)!="]") {
          while(ok) {ok=text();space();if(substr(data,pos,1)!=",") break;pos++;space()}
        }
        if(substr(data,pos++,1)!="]") ok=0;
      }
      space();exit !(ok&&pos>length(data));
    }'
}
metadata() {
    metadata_value=$(property "$1" "$2")
    metadata_type=string
    [ "$2" = labels ] && metadata_type=labels
    if valid_metadata "$metadata_value" "$metadata_type"; then raw "$metadata_value"; else unknown "$3"; fi
}
runners() {
    if [ "$linux" != true ] || ! command -v systemctl >/dev/null 2>&1; then unknown 'systemd unavailable'; return; fi
    loaded_ok=true; installed_ok=true
    units=$(systemctl list-units --all --plain --no-legend 'actions.runner.*' 2>/dev/null) || loaded_ok=false
    installed=$(systemctl list-unit-files --no-legend 'actions.runner.*' 2>/dev/null) || installed_ok=false
    if [ "$loaded_ok" = false ] && [ "$installed_ok" = false ]; then unknown 'systemd runner enumeration unavailable'; return; fi
    printf '{"value":['; runner_sep=''
    printf '%s\n%s\n' "$units" "$installed" | awk '$1~/^actions[.]runner[.]/ {print $1}' | sort -u | while IFS= read -r unit; do
        printf '%s{' "$runner_sep"; runner_sep=','
        runner_dir=$(systemctl show "$unit" -p WorkingDirectory --value 2>/dev/null)
        # Only a successful empty User property means systemd's default root.
        if runner_user=$(systemctl show "$unit" -p User --value 2>/dev/null); then
            [ -n "$runner_user" ] || runner_user=root
        else runner_user=''; fi
        # An absent WorkingDirectory must not accidentally read /.runner.
        [ -n "$runner_dir" ] || runner_dir=/dev/null
        runner_home=$(getent passwd "$runner_user" 2>/dev/null | awk -F: '{print $6}')
        pid=$(systemctl show "$unit" -p MainPID --value 2>/dev/null)
        field unit; string "$unit" 'unit unavailable'; comma
        field scope; metadata "$runner_dir/.runner" gitHubUrl 'scope metadata unreadable or absent'; comma
        field name; metadata "$runner_dir/.runner" agentName 'runner metadata unreadable or absent'; comma
        field labels; metadata "$runner_dir/.runner" labels 'labels not recorded in .runner'; comma
        field ephemeral
        if [ -r "$runner_dir/.runner" ]; then
            ephemeral=$(property "$runner_dir/.runner" ephemeral)
            # RunnerSettings omits the default false flag when persisting .runner.
            case "$ephemeral" in true|false) raw "$ephemeral";; '') raw false;; *) unknown 'ephemeral metadata is invalid';; esac
        else unknown 'runner metadata unreadable or absent'; fi; comma
        field user; string "$runner_user" 'unit user unavailable'; comma
        field home; string "$runner_home" 'user home unavailable'; comma
        field version
        version=$(awk 'match($0,/Runner[.]Listener\/[0-9][^" ]*/) {v=substr($0,RSTART+16,RLENGTH-16);print v;exit}' "$runner_dir/bin/Runner.Listener.deps.json" 2>/dev/null)
        string "$version" 'runner version manifest unreadable or absent'; comma
        field release_age_days; unknown 'release publication date requires local release metadata'; comma
        field active_job
        if [ "$pid" = 0 ]; then raw false
        elif printf '%s' "$pid" | awk '/^[1-9][0-9]*$/ {ok=1} END {exit !ok}'; then
            if process_tree=$(ps -eo pid=,ppid=,comm= 2>/dev/null); then
                active=$(printf '%s\n' "$process_tree" | awk -v root="$pid" '
                {parent[$1]=$2;worker[$1]=($3~/Runner[.]Worker/)}
                END {for(p in worker) if(worker[p]) {x=p;for(i=0;i<1000&&x>1;i++) {if(x==root) {print "true";exit} x=parent[x]}} print "false"}')
                raw "$active"
            else unknown 'process tree unavailable'; fi
        else unknown 'unit process ID unavailable'; fi
        printf '}'
    done
    printf '],"unknown_reason":null}'
}
daily_cpu() {
    if ! command -v sar >/dev/null 2>&1; then unknown 'sar is not installed'; return; fi
    daily_sep=''; daily_data=''
    # Today (partial) plus the six previous days, so a fresh sysstat install reports data.
    for offset in 0 1 2 3 4 5 6; do
        day=$(date -d "$offset days ago" +%Y-%m-%d 2>/dev/null)
        [ -n "$day" ] || continue
        compact=$(printf '%s' "$day" | tr -d '-')
        short=$(printf '%s' "$day" | cut -c9-10)
        sa_file=''
        for candidate in "/var/log/sa/sa$compact" "/var/log/sysstat/sa$compact" "/var/log/sa/sa$short" "/var/log/sysstat/sa$short"; do
            if [ -r "$candidate" ]; then sa_file=$candidate; break; fi
        done
        [ -n "$sa_file" ] || continue
        # Reject mismatched dates (old monthly files) and nonaggregate rows.
        stats=$(sar -u -f "$sa_file" 2>/dev/null | awk -v date="$day" '
          NR==1 {split(date,d,"-"); expected=d[2] "/" d[3] "/" d[1]; short=d[2] "/" d[3] "/" substr(d[1],3); for(i=1;i<=NF;i++) if($i==date||$i==expected||$i==short) valid=1}
          valid && $1!="Average:" && /[[:space:]]all[[:space:]]/ && $NF~/^[0-9]+([.][0-9]+)?$/ {printf "%.6f\n",100-$NF}' |
          sort -n | awk '{v[++n]=$1} END {if(n) {p50=int(n*.5+.999999);p95=int(n*.95+.999999);printf "%s %s %d",v[p50],v[p95],n}}')
        [ -n "$stats" ] || continue
        # shellcheck disable=SC2086 # Numeric tokens, intentional splitting; globbing disabled.
        set -- $stats
        daily_data="$daily_data$daily_sep{\"date\":{\"value\":\"$day\",\"unknown_reason\":null},\"p50\":{\"value\":$1,\"unknown_reason\":null},\"p95\":{\"value\":$2,\"unknown_reason\":null},\"samples\":{\"value\":$3,\"unknown_reason\":null}}"
        daily_sep=','
    done
    if [ -n "$daily_data" ]; then raw "[$daily_data]"; else unknown 'no readable CPU samples in the last seven days'; fi
}
listeners() {
    if ! command -v ss >/dev/null 2>&1; then unknown 'ss unavailable; listening processes unknown'; return; fi
    if ! sockets=$(read_privileged ss -H -lntup); then unknown 'listening socket enumeration denied'; return; fi
    # Report listening process identities, not arbitrary command lines.
    printf '{"value":['; listen_sep=''
    printf '%s\n' "$sockets" | awk '{name=""; if(match($0,/users:\(\("[^"]*"/)) name=substr($0,RSTART+9,RLENGTH-10); if(NF>=6) print $5 "\t" name}' |
    while IFS="$(printf '\t')" read -r endpoint service; do
        printf '%s{' "$listen_sep"; listen_sep=','
        field name; string "$service" 'process identity unavailable without privilege'; comma
        field endpoint; string "$endpoint" 'socket endpoint unavailable'; printf '}'
    done
    printf '],"unknown_reason":null}'
}

printf '{'
field os
os=$(awk -F= '$1=="PRETTY_NAME" {v=substr($0,index($0,"=")+1);sub(/^"/,"",v);sub(/"$/,"",v);print v;exit}' /etc/os-release 2>/dev/null)
string "$os" 'Linux os-release unavailable'; comma
field kernel; string "$(uname -r 2>/dev/null)" 'uname unavailable'; comma
field arch; string "$(uname -m 2>/dev/null)" 'uname unavailable'; comma
field vcpus; number "$(getconf _NPROCESSORS_ONLN 2>/dev/null)" 'CPU count unavailable'; comma
field cpu_model; string "$(awk -F: '/^model name|^Hardware|^Processor/ {sub(/^[ \t]+/,"",$2);print $2;exit}' /proc/cpuinfo 2>/dev/null)" 'Linux CPU metadata unavailable'; comma
field hypervisor
if command -v systemd-detect-virt >/dev/null 2>&1; then string "$(systemd-detect-virt 2>/dev/null)" 'virtualization detection unavailable'
else unknown 'systemd-detect-virt unavailable'; fi; comma
field kvm; if [ "$linux" = true ]; then if [ -c /dev/kvm ]; then raw true; else raw false; fi; else unknown 'not Linux'; fi; comma
field ram_bytes; number "$(mem MemTotal)" 'Linux meminfo unavailable'; comma
field swap_total_bytes; number "$(mem SwapTotal)" 'Linux meminfo unavailable'; comma
field swap_used_bytes
swap_total=$(mem SwapTotal); swap_free=$(mem SwapFree)
if [ -n "$swap_total" ] && [ -n "$swap_free" ]; then number "$((swap_total - swap_free))" 'swap usage unavailable'; else unknown 'Linux meminfo unavailable'; fi; comma
field swappiness; number "$(cat /proc/sys/vm/swappiness 2>/dev/null)" 'Linux swappiness unavailable'; comma
field root_fs_type; string "$(findmnt -n -o FSTYPE / 2>/dev/null)" 'findmnt unavailable'; comma
root_df=$(df -Pk / 2>/dev/null | awk 'NR==2 {printf "%.0f %.0f",$2*1024,$4*1024}')
field root_size_bytes; number "$(printf '%s' "$root_df" | awk '{print $1}')" 'root size unavailable'; comma
field root_free_bytes; number "$(printf '%s' "$root_df" | awk '{print $2}')" 'root free space unavailable'; comma
field disk_scheduler
root_device=$(findmnt -n -o SOURCE / 2>/dev/null)
root_block=$(lsblk -no PKNAME "$root_device" 2>/dev/null | awk 'NF {print $1;exit}')
[ -n "$root_block" ] || root_block=${root_device##*/}
string "$(cat "/sys/class/block/$root_block/queue/scheduler" 2>/dev/null)" 'root block scheduler unavailable'; comma
field cgroup_v2; if [ "$linux" = true ]; then if [ -f /sys/fs/cgroup/cgroup.controllers ]; then raw true; else raw false; fi; else unknown 'not Linux'; fi; comma
field controllers; if [ -r /sys/fs/cgroup/cgroup.controllers ]; then raw "$(words "$(cat /sys/fs/cgroup/cgroup.controllers)")"; else unknown 'cgroup v2 controllers unavailable'; fi; comma
field enabled_controllers; if [ -r /sys/fs/cgroup/cgroup.subtree_control ]; then raw "$(words "$(cat /sys/fs/cgroup/cgroup.subtree_control)")"; else unknown 'cgroup v2 enabled controllers unavailable'; fi; comma
field psi; if [ "$linux" = true ]; then if [ -r /proc/pressure/cpu ]; then raw true; else raw false; fi; else unknown 'not Linux'; fi; comma
field pressure; printf '{"value":{'
pressure_sep=''
for resource in cpu memory io; do for kind in some full; do printf '%s' "$pressure_sep"; pressure_sep=','; field "${resource}_${kind}"; psi "$resource" "$kind"; done; done
printf '},"unknown_reason":null}'; comma
field systemd_version; string "$(systemctl --version 2>/dev/null | awk 'NR==1 {print $2}')" 'systemd unavailable'; comma
field docker_present
if command -v docker >/dev/null 2>&1; then raw true; docker=true; else raw false; docker=false; fi; comma
field docker_version; string "$(docker --version 2>/dev/null)" 'Docker CLI unavailable'; comma
field docker_cgroup_driver
if [ "$docker" = true ] && [ "$linux" = true ]; then driver=$(read_privileged docker info --format '{{.CgroupDriver}}'); else driver=''; fi
string "$driver" 'Docker daemon unavailable or access denied'; comma
field docker_root_dir
if [ "$docker" = true ] && [ "$linux" = true ]; then docker_root=$(read_privileged docker info --format '{{.DockerRootDir}}'); else docker_root=''; fi
string "$docker_root" 'Docker daemon unavailable or access denied'; comma
field docker_root_usage_bytes
# du over a large Docker root can take minutes; give up after 10 s and report it as unknown.
if command -v timeout >/dev/null 2>&1; then bounded='timeout 10'; else bounded=''; fi
if [ -n "$docker_root" ]; then usage=$(read_privileged $bounded du -sk "$docker_root" | awk '{printf "%.0f",$1*1024}'); else usage=''; fi
number "$usage" 'Docker root unavailable, unreadable or too large to measure quickly'; comma
field runners; runners; comma
field load_average
load=$(awk '{printf "[%s,%s,%s]",$1,$2,$3}' /proc/loadavg 2>/dev/null)
if [ -n "$load" ]; then raw "$load"; else unknown 'Linux load average unavailable'; fi; comma
field sar_installed; if command -v sar >/dev/null 2>&1; then raw true; else raw false; fi; comma
field daily_cpu; daily_cpu; comma
field listening_services; listeners; comma
field sudo_available; raw "$privileged"
printf '}\n'
