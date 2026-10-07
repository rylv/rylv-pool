# Consume critcmp 0.1.8 --list --color never output, first unfiltered,
# then filtered with --threshold. Criterion data and statistics stay in critcmp.
# Usage: awk -f benchmark_gate.awk all.txt significant.txt

function invalid(message) {
    print "Invalid critcmp report: " message > "/dev/stderr"
    errors = 1
}

function baseline(line, fields, count) {
    count = split(line, fields, /[ \t]+/)
    if (count != 5 || (fields[1] != "base" && fields[1] != "pr") ||
        fields[2] !~ /^[0-9]+\.[0-9][0-9]$/ || fields[2] + 0 < 1 ||
        fields[3] !~ /^[0-9]+\.[0-9]±[0-9]+\.[0-9][0-9](ns|µs|ms|s)$/) {
        invalid("unexpected benchmark row for " name)
        return ""
    }
    return fields[1]
}

BEGIN {
    RS = ""
    FS = "\n"
    if (ARGC != 3 || ARGV[1] == ARGV[2]) {
        invalid("expected two different files: unfiltered and threshold-filtered")
        exit 2
    }
}

{
    for (i = 1; i <= NF; i++) sub(/\r$/, "", $i)
    name = $1
    if (NF < 3 || NF > 4 || $2 !~ /^-+$/) {
        invalid("unexpected comparison block for " name)
        next
    }
    first = baseline($3)
    second = NF == 4 ? baseline($4) : ""
    if (!first || (NF == 4 && (!second || first == second))) {
        invalid("missing or duplicate baseline for " name)
        next
    }

    if (FILENAME == ARGV[1]) {
        if (name in full_order) {
            invalid("duplicate comparison for " name)
            next
        }
        full_order[name] = first
        if (second) {
            paired[name] = 1
            paired_count++
        } else if (first == "base") {
            removed_count++
            print "Removed case: " name
        } else {
            added_count++
            print "Added case: " name
        }
    } else {
        if (!second || !(name in paired) || name in significant || full_order[name] != first) {
            invalid("threshold report does not match a paired comparison for " name)
            next
        }
        significant[name] = 1
        # critcmp sorts by exact means, fastest first, before rounding its ranks.
        # The threshold already selects differences strictly above the limit.
        if (first == "base" && second == "pr") {
            regressions++
            print "Regression above threshold: " name
        }
    }
}

END {
    if (errors) exit 2
    if (!paired_count) {
        invalid("no benchmark IDs exist in both base and pr")
        exit 2
    }
    printf "Paired cases: %d; added: %d; removed: %d; regressions: %d\n", \
        paired_count, added_count, removed_count, regressions
    if (regressions) exit 1
}
