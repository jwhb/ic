#!/usr/bin/env bash

set -o nounset
set -o pipefail

SHELL="/bin/bash"
PATH="/sbin:/bin:/usr/sbin:/usr/bin"

source /opt/ic/bin/functions.sh

CONFIG="${CONFIG:=/var/ic/config/config.ini}"
DEPLOYMENT="${DEPLOYMENT:=/data/deployment.json}"

function read_variables() {
    # Read limited set of keys. Be extra-careful quoting values as it could
    # otherwise lead to executing arbitrary shell code!
    while IFS="=" read -r key value; do
        case "$key" in
            "ipv6_prefix") ipv6_prefix="${value}" ;;
            "ipv6_gateway") ipv6_gateway="${value}" ;;
            "ipv4_address") ipv4_address="${value}" ;;
            "ipv4_prefix_length") ipv4_prefix_length="${value}" ;;
            "ipv4_gateway") ipv4_gateway="${value}" ;;
            "domain") domain="${value}" ;;
        esac
    done <"${CONFIG}"
}

# WARNING: Uses 'eval' for command execution.
# Ensure 'command' is a trusted, fixed string.
function eval_command_with_retries() {
    local command="${1}"
    local error_message="${2}"
    local result=""
    local attempt_count=0
    local exit_code=1

    while [ ${exit_code} -ne 0 ] && [ ${attempt_count} -lt 3 ]; do
        result=$(eval "${command}")
        exit_code=$?
        ((attempt_count++))

        if [ ${exit_code} -ne 0 ] && [ ${attempt_count} -lt 3 ]; then
            sleep 1
        fi
    done

    if [ ${exit_code} -ne 0 ]; then
        local ip6_output=$(ip -6 addr show)
        local ip6_route_output=$(ip -6 route show)
        local dns_servers=$(grep 'nameserver' /etc/resolv.conf)

        log_and_halt_installation_on_error "${exit_code}" "${error_message}
Output of 'ip -6 addr show':
${ip6_output}

Output of 'ip -6 route show':
${ip6_route_output}

Configured DNS servers:
${dns_servers}"
    fi

    echo "${result}"
}

function get_network_settings() {
    ipv6_capable_interfaces=$(eval_command_with_retries \
        "ip -6 addr show | awk '/^[0-9]+: / {print \$2}' | sed 's/://g' | grep -v '^lo$'" \
        "Failed to get system's network interfaces.")

    if [ -z "${ipv6_capable_interfaces}" ]; then
        log_and_halt_installation_on_error "1" "No network interfaces with IPv6 addresses found."
    else
        echo "IPv6-capable interfaces found:"
        echo "${ipv6_capable_interfaces}"
    fi

    # Full IPv6 address
    ipv6_address_system_full=$(eval_command_with_retries \
        "ip -6 addr show | awk '(/inet6/) && (!/\sfe80|\s::1/) { print \$2 }'" \
        "Failed to get system's network configuration.")

    if [ -z "${ipv6_address_system_full}" ]; then
        log_and_halt_installation_on_error "1" "No IPv6 addresses found."
    fi

    ipv6_prefix_system=$(eval_command_with_retries \
        "echo ${ipv6_address_system_full} | cut -d: -f1-4" \
        "Failed to get system's IPv6 prefix.")

    ipv6_subnet_system=$(eval_command_with_retries \
        "echo ${ipv6_address_system_full} | awk -F '/' '{ print \"/\" \$2 }'" \
        "Failed to get system's IPv6 subnet.")

    ipv6_gateway_system=$(eval_command_with_retries \
        "ip -6 route show | awk '(/^default/) { print \$3 }'" \
        "Failed to get system's IPv6 gateway.")

    ipv6_address_system=$(eval_command_with_retries \
        "echo ${ipv6_address_system_full} | awk -F '/' '{ print \$1 }'" \
        "Failed to get system's IPv6 address.")

    HOSTOS_IPV6_ADDRESS=$(/opt/ic/bin/setupos_tool generate-ipv6-address --node-type HostOS)
    GUESTOS_IPV6_ADDRESS=$(/opt/ic/bin/setupos_tool generate-ipv6-address --node-type GuestOS)
}

function print_network_settings() {
    echo "* Printing user defined network settings..."
    echo "  IPv6 Prefix : ${ipv6_prefix}"
    echo "  IPv6 Gateway: ${ipv6_gateway}"
    if [[ -n ${ipv4_address} && -n ${ipv4_prefix_length} && -n ${ipv4_gateway} && -n ${domain} ]]; then
        echo "  IPv4 Address: ${ipv4_address}"
        echo "  IPv4 Prefix Length: ${ipv4_prefix_length}"
        echo "  IPv4 Gateway: ${ipv4_gateway}"
        echo "  Domain name : ${domain}"
    fi
    echo " "

    echo "* Printing system's network settings..."
    echo "  IPv6 Prefix : ${ipv6_prefix_system}"
    echo "  IPv6 Subnet : ${ipv6_subnet_system}"
    echo "  IPv6 Gateway: ${ipv6_gateway_system}"
    echo " "

    echo "* Printing IPv6 addresses..."
    echo "  SetupOS: ${ipv6_address_system_full}"
    echo "  HostOS : ${HOSTOS_IPV6_ADDRESS}"
    echo "  GuestOS: ${GUESTOS_IPV6_ADDRESS}"
    echo " "
}

function validate_domain_name() {
    local domain_part
    local -a domain_parts

    IFS='.' read -ra domain_parts <<<"${domain}"

    if [ ${#domain_parts[@]} -lt 2 ]; then
        log_and_halt_installation_on_error 1 "Domain validation error: less than two domain parts in domain: ${domain}"
    fi

    for domain_part in "${domain_parts[@]}"; do
        if [ -z "$domain_part" ] || [ ${#domain_part} -gt 63 ]; then
            log_and_halt_installation_on_error 1 "Domain validation error: domain part length violation: ${domain_part}"
        fi

        if [[ $domain_part == -* ]] || [[ $domain_part == *- ]]; then
            log_and_halt_installation_on_error 1 "Domain validation error: domain part starts or ends with a hyphen: ${domain_part}"
        fi

        if ! [[ $domain_part =~ ^[a-zA-Z0-9-]+$ ]]; then
            log_and_halt_installation_on_error 1 "Domain validation error: invalid characters in domain part: ${domain_part}"
        fi
    done
}

function setup_ipv4_network() {
    echo "* Setting up IPv4 network..."

    ip addr add ${ipv4_address}/${ipv4_prefix_length} dev 'br6'
    log_and_halt_installation_on_error "${?}" "Unable to add IPv4 address to interface."

    ip route add default via ${ipv4_gateway}
    log_and_halt_installation_on_error "${?}" "Unable to set default route in IPv4 network configuration."
}

function ping_ipv4_gateway() {
    echo "* Pinging IPv4 gateway..."
    # wait 20 seconds maximum for any network changes to settle.
    ping4 -c 2 -w 20 ${ipv4_gateway} >/dev/null 2>&1
    log_and_halt_installation_on_error "${?}" "Unable to ping IPv4 gateway."

    echo "  success"
}

function ping_ipv6_gateway() {
    echo "* Pinging IPv6 gateway..."

    ping6 -c 4 ${ipv6_gateway_system} >/dev/null 2>&1
    log_and_halt_installation_on_error "${?}" "Unable to ping IPv6 gateway."

    echo "  success"
    echo " "
}

function assemble_nns_nodes_list() {
    NNS_URL_STRING=$(/opt/ic/bin/fetch-property.sh --key=.nns.url --config=${DEPLOYMENT})
    IFS=',' read -r -a NNS_URL_LIST <<<"$NNS_URL_STRING"
}

function query_nns_nodes() {
    echo "* Querying NNS nodes..."

    local success=false
    # At least one of the provided URLs needs to work.
    for url in "${NNS_URL_LIST[@]}"; do
        # When running against testnets, we need to ignore self signed certs
        # with `--insecure`. This check is only meant to confirm from SetupOS
        # that NNS urls are reachable, so we do not mind that it is "weak".
        if curl --insecure --head --connect-timeout 3 --silent "${url}" >/dev/null 2>&1; then
            echo "  okay: ${url}"
            success=true
            break
        else
            echo "  fail: ${url}"
        fi
    done

    if $success; then
        echo "  success"
    else
        log_and_halt_installation_on_error "1" "Unable to query enough healthy NNS nodes."
    fi
}

# Establish run order
main() {
    log_start "$(basename $0)"
    read_variables
    get_network_settings
    print_network_settings

    if [[ -n ${ipv4_address} && -n ${ipv4_prefix_length} && -n ${ipv4_gateway} ]]; then
        validate_domain_name
        setup_ipv4_network
        ping_ipv4_gateway
    fi

    ping_ipv6_gateway
    assemble_nns_nodes_list
    query_nns_nodes
    log_end "$(basename $0)"
}

main
