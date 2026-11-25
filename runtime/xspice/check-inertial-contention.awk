# Verify the entire four-output transition trace rather than isolated samples.
# A late stale event or short glitch otherwise might go undetected.
function level(v) {
    return v < 0.2 ? "L" : v > 0.8 ? "H" : "U"
}
function abs(v) { return v < 0 ? -v : v }
function observe(channel, value,    ordinal, lookup) {
    if (value == previous[channel]) return
    ordinal = ++transitions[channel]
    lookup = channel SUBSEP ordinal
    if (!(lookup in expected_state) || value != expected_state[lookup] ||
        abs(time_ns - expected_time[lookup]) > 0.1) {
        printf "Unexpected %s transition at %.4f ns: %s -> %s (#%d)\n", \
            channel, time_ns, previous[channel], value, ordinal > "/dev/stderr"
        failed = 1
    }
    previous[channel] = value
}
BEGIN {
    previous["inertial"] = "L"
    previous["release"] = "U"
    previous["loaded"] = "L"
    previous["transport"] = "L"

    expected_state["inertial", 1] = "U"; expected_time["inertial", 1] = 12.002
    expected_state["inertial", 2] = "L"; expected_time["inertial", 2] = 26.002
    expected_state["release", 1] = "H"; expected_time["release", 1] = 12.002
    expected_state["release", 2] = "U"; expected_time["release", 2] = 26.002

    expected_state["transport", 1] = "U"; expected_time["transport", 1] = 6.002
    expected_state["transport", 2] = "L"; expected_time["transport", 2] = 8.002
    expected_state["transport", 3] = "U"; expected_time["transport", 3] = 12.002
    expected_state["transport", 4] = "L"; expected_time["transport", 4] = 20.002
    expected_state["transport", 5] = "U"; expected_time["transport", 5] = 24.002
    expected_state["transport", 6] = "L"; expected_time["transport", 6] = 26.002
}
{
    if (NF != 8) {print "Malformed wrdata row" > "/dev/stderr"; exit 1}
    time_ns = $1 * 1e9
    if (NR == 1) {
        if (level($2) != previous["inertial"] ||
            level($4) != previous["release"] ||
            level($6) != previous["loaded"] ||
            level($8) != previous["transport"]) {
            print "Incorrect initial contention states" > "/dev/stderr"
            exit 1
        }
        next
    }
    observe("inertial", level($2))
    observe("release", level($4))
    observe("loaded", level($6))
    observe("transport", level($8))
}
END {
    if (NR < 1000 || transitions["inertial"] != 2 ||
        transitions["release"] != 2 || transitions["loaded"] != 0 ||
        transitions["transport"] != 6) {
        print "Wrong inertial/transport transition count" > "/dev/stderr"
        failed = 1
    }
    if (failed) exit 1
}
