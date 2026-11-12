# lig: run this interactive PowerShell inside lig (once per console), which loads its
# integration after the profile. Scripts (-Command, -File, …) and other hosts run as usual.
if (-not $env:LIG_DISABLE -and -not $env:LIG_PID -and $Host.Name -eq 'ConsoleHost' -and
    -not [Console]::IsInputRedirected -and -not [Console]::IsOutputRedirected) {
    $__lig_script = [Environment]::GetCommandLineArgs() | Select-Object -Skip 1 |
        Where-Object { $_ -match '^[-/](c(o(m(m(a(nd?)?)?)?)?)?|f(i(le?)?)?|e|ec|en\w*|noni\w*|noe\w*)$' }
    if (-not $__lig_script) {
        & '@LIG_BIN@' shell --shell ([Diagnostics.Process]::GetCurrentProcess().MainModule.FileName)
        exit $LASTEXITCODE
    }
    Remove-Variable __lig_script
}
