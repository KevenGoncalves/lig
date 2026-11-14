# Specs for nu's own commands, from their signatures, in lig's JSON format: `ls` here is nu's,
# not coreutils'. Subcommands (`str replace`) come flat, by their full names.
scope commands | where type in ["built-in" "keyword" "plugin"] | each {|c|
    let params = $c.signatures | values | first | default []
    let options = $params | where parameter_type in ["switch" "named"] | each {|p|
        let long = $"--($p.parameter_name)"
        let names = if ($p.short_flag | is-empty) { [$long] } else { [$"-($p.short_flag)" $long] }
        let option = { name: $names, description: ($p.description | default "") }
        if $p.parameter_type == "named" {
            $option | insert args { name: ($p.syntax_shape | default "value") }
        } else {
            $option
        }
    }
    let args = $params | where parameter_type in ["positional" "rest"] | each {|p|
        let shape = $p.syntax_shape | default ""
        let arg = { name: $p.parameter_name, isOptional: ($p.is_optional or $p.parameter_type == "rest") }
        let arg = if $p.parameter_type == "rest" { $arg | insert isVariadic true } else { $arg }
        if ($shape | str contains "directory") {
            $arg | insert template "folders"
        } else if ($shape =~ "path|glob") {
            $arg | insert template "filepaths"
        } else {
            $arg
        }
    }
    { name: $c.name, description: ($c.description | default ""), options: $options, args: $args }
} | to json --raw
