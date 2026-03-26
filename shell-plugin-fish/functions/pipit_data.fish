# ──────────────────────────────────────────────────────────────────────
#  pipit_data — JSONL batch processing with AI
# ──────────────────────────────────────────────────────────────────────
#
#  Fish-native approach:
#    - Reads JSONL input (file or stdin)
#    - Processes each line through the AI with a template prompt
#    - Writes JSONL output with AI-generated fields
#    - Parallel processing via Fish's `&` and job control
#
#  Usage:
#    pipit data --input data.jsonl --template "Classify: {text}" --output out.jsonl
#    cat data.jsonl | pipit data --template "Summarize: {content}"
#    pipit data --input data.jsonl --template prompt.txt --parallel 4
#
# ──────────────────────────────────────────────────────────────────────
function pipit_data -d "JSONL batch processing with AI"
    set -l input_file ""
    set -l output_file ""
    set -l template ""
    set -l parallel_count 1
    set -l field "result"

    # Parse arguments
    set -l i 1
    while test $i -le (count $argv)
        switch $argv[$i]
            case --input -i
                set i (math $i + 1)
                set input_file $argv[$i]
            case --output -o
                set i (math $i + 1)
                set output_file $argv[$i]
            case --template -t
                set i (math $i + 1)
                set template $argv[$i]
            case --parallel -p
                set i (math $i + 1)
                set parallel_count $argv[$i]
            case --field -f
                set i (math $i + 1)
                set field $argv[$i]
            case --help -h
                _pipit_data_help
                return
            case '*'
                _pipit_log error "Unknown argument: $argv[$i]"
                _pipit_data_help
                return 1
        end
        set i (math $i + 1)
    end

    if test -z "$template"
        _pipit_log error "Template is required."
        _pipit_data_help
        return 1
    end

    # If template is a file path, read its content
    if test -f "$template"
        set template (cat $template)
    end

    # Read input lines
    set -l lines
    if test -n "$input_file"
        if not test -f "$input_file"
            _pipit_log error "Input file not found: $input_file"
            return 1
        end
        set lines (cat $input_file)
    else
        # Read from stdin
        while read -l line
            set -a lines $line
        end
    end

    if test (count $lines) -eq 0
        _pipit_log error "No input lines."
        return 1
    end

    set -l total (count $lines)
    _pipit_log info "Processing $total lines (parallel: $parallel_count)..."

    # Process each line
    set -l results
    set -l processed 0
    set -l tmpdir (mktemp -d /tmp/pipit-data-XXXXXX)

    for line in $lines
        set processed (math $processed + 1)

        # Render template: replace {field} with JSON field values
        # Simple approach: extract top-level string fields with string match
        set -l rendered $template
        # Match {word} patterns and try to extract from JSON
        for placeholder in (string match -ra '\{[a-zA-Z_]+\}' -- $rendered)
            set -l key (string replace -r '^\{|\}$' '' -- $placeholder)
            # Extract value from JSON: look for "key": "value" or "key": value
            set -l val (string match -r '"'$key'"\\s*:\\s*"([^"]*)"' -- $line)
            if test (count $val) -ge 2
                set rendered (string replace -- $placeholder $val[2] $rendered)
            end
        end

        # Call AI
        set -l result_file "$tmpdir/$processed.out"
        if test $parallel_count -gt 1
            _pipit_exec prompt "$rendered" > $result_file 2>/dev/null &
        else
            _pipit_exec prompt "$rendered" > $result_file 2>/dev/null
            set -l ai_result (cat $result_file)
            # Escape for JSON
            set ai_result (string replace -a '"' '\\"' -- $ai_result)
            set ai_result (string replace -a \n '\\n' -- $ai_result)

            # Append field to original JSON
            set -l output_line (string replace -r '\}$' ',"'$field'":"'$ai_result'"}' -- $line)
            set -a results $output_line

            # Progress
            printf "\r  %d/%d" $processed $total >&2
        end

        # Throttle parallel jobs
        if test $parallel_count -gt 1
            while test (jobs -p | wc -l | string trim) -ge $parallel_count
                sleep 0.1
            end
        end
    end

    # Wait for parallel jobs
    if test $parallel_count -gt 1
        wait
        # Collect results in order
        for idx in (seq 1 $total)
            set -l result_file "$tmpdir/$idx.out"
            if test -f $result_file
                set -l ai_result (cat $result_file)
                set ai_result (string replace -a '"' '\\"' -- $ai_result)
                set ai_result (string replace -a \n '\\n' -- $ai_result)
                set -l output_line (string replace -r '\}$' ',"'$field'":"'$ai_result'"}' -- $lines[$idx])
                set -a results $output_line
            end
        end
    end

    echo >&2  # Clear progress line

    # Output
    if test -n "$output_file"
        printf '%s\n' $results > $output_file
        _pipit_log ok "Wrote $total results to $output_file"
    else
        printf '%s\n' $results
    end

    # Cleanup
    command rm -rf $tmpdir
end

function _pipit_data_help
    echo
    set_color --bold cyan
    echo "  pipit data — JSONL batch processing with AI"
    set_color normal
    echo
    echo "  Usage:"
    echo "    pipit data --input data.jsonl --template \"Classify: {text}\" --output out.jsonl"
    echo "    cat data.jsonl | pipit data --template \"Summarize: {content}\""
    echo "    pipit data -i data.jsonl -t prompt.txt -p 4 -f classification"
    echo
    echo "  Flags:"
    printf "    %-20s %s\n" "--input, -i"    "Input JSONL file (or pipe from stdin)"
    printf "    %-20s %s\n" "--output, -o"   "Output JSONL file (or stdout)"
    printf "    %-20s %s\n" "--template, -t" "Prompt template (string or file path)"
    printf "    %-20s %s\n" "--parallel, -p" "Concurrent requests (default: 1)"
    printf "    %-20s %s\n" "--field, -f"    "Output field name (default: 'result')"
    echo
    echo "  Template variables:"
    echo "    {field_name}  → replaced with JSON field value from each line"
    echo
end
