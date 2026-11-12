# lig: PowerShell integration, dot-sourced by lig after the profile (see src/session.rs).
# It speaks fish's protocol (see shell/post.fish, src/osc.rs): lig asks for the line with
# Ctrl+Alt+Shift+F11 and inserts with Ctrl+Alt+Shift+F12; replies are OSC 6973 messages.
# Written for Windows PowerShell 5.1 too, which has no `e escape.
if ($env:LIG_PID -and -not $env:LIG_DISABLE -and -not $global:__lig_loaded -and
    (Get-Command Set-PSReadLineKeyHandler -ErrorAction SilentlyContinue)) {
    $global:__lig_loaded = $true
    $global:__lig_editing = $false
    $global:__lig_aliases = $null
    $global:__lig_vars = $null
    $global:__lig_once = $false

    function global:__lig_send([string] $msg) {
        [Console]::Write([string][char]27 + ']6973;' + ($msg -replace '[\x07\x1b]', ' ') + [char]7)
    }

    $global:__lig_prompt = $function:prompt
    function global:prompt {
        $loc = Get-Location
        if ($loc.Provider.Name -eq 'FileSystem') { __lig_send ('C;' + $loc.ProviderPath) }
        __lig_send ('P;' + $env:PATH)
        $sep = [string][char]0x1e
        $aliases = (Get-Alias | ForEach-Object { $_.Name + '=' + $_.Definition }) -join $sep
        if ($aliases -ne $global:__lig_aliases) {
            $global:__lig_aliases = $aliases
            __lig_send ('A;' + $aliases)
        }
        # `$env:` names for the environment, plain ones for PowerShell's own simple values.
        $vars = @(Get-ChildItem env: | ForEach-Object { 'env:' + $_.Name + '=' + $_.Value })
        $vars += Get-Variable -Scope Global | Where-Object {
            $_.Name -match '^\w+$' -and ($_.Value -is [string] -or $_.Value -is [ValueType])
        } | ForEach-Object { $_.Name + '=' + $_.Value }
        $vars = ($vars -join $sep) -replace '[\r\n]', ' '
        if ($vars -ne $global:__lig_vars) {
            $global:__lig_vars = $vars
            __lig_send ('V;' + $vars)
        }
        if (-not $global:__lig_once) {
            $global:__lig_once = $true
            $cmdlets = Get-Command -CommandType Cmdlet -ListImported -ErrorAction SilentlyContinue
            __lig_send ('K;' + (($cmdlets | ForEach-Object Name) -join $sep))
            $functions = Get-Command -CommandType Function -ListImported -ErrorAction SilentlyContinue |
                Where-Object { $_.Name -notmatch '^_|:' }
            __lig_send ('N;' + (($functions | ForEach-Object Name) -join $sep))
        }
        $text = & $global:__lig_prompt
        $global:__lig_editing = $true
        # Ready: lig may ask for the line now.
        __lig_send "R;$PID"
        $text
    }

    # Enter: tell lig the line runs, then do what Enter did before.
    $global:__lig_enter = 'AcceptLine'
    # PSReadLine 2.0 (Windows PowerShell 5.1) has no -Chord here: look Enter up in the list.
    $old = Get-PSReadLineKeyHandler | Where-Object { $_.Key -eq 'Enter' } | Select-Object -First 1
    if ($old -and ([Microsoft.PowerShell.PSConsoleReadLine] | Get-Member -Static -Name $old.Function)) {
        $global:__lig_enter = $old.Function
    }
    Remove-Variable old -ErrorAction SilentlyContinue
    Set-PSReadLineKeyHandler -Chord Enter -ScriptBlock {
        param($key, $arg)
        if ($global:__lig_editing) {
            $global:__lig_editing = $false
            __lig_send 'F'
        }
        $name = $global:__lig_enter
        [Microsoft.PowerShell.PSConsoleReadLine]::$name($key, $arg)
    }

    # lig wrote a request id to $env:LIG_QUERY_FILE: reply with the line and the cursor.
    Set-PSReadLineKeyHandler -Chord 'Ctrl+Alt+Shift+F11' -ScriptBlock {
        if (-not $global:__lig_editing) { return }
        try { $id = [IO.File]::ReadAllText($env:LIG_QUERY_FILE).Trim() } catch { return }
        if ($id -notmatch '^[0-9]+$') { return }
        $line = $null; $cursor = $null
        [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$cursor)
        __lig_send "Q;$id;B;$cursor;$line"
    }

    # Payload in $env:LIG_INSERT_FILE: delete;back;prefix-length;prefix+insertion. The insertion
    # is for the line lig saw: if the text left of the cursor changed since, say so (J) instead.
    Set-PSReadLineKeyHandler -Chord 'Ctrl+Alt+Shift+F12' -ScriptBlock {
        try { $payload = [IO.File]::ReadAllText($env:LIG_INSERT_FILE) } catch { return }
        $f = $payload.Split([char[]]';', 4)
        $delete = 0; $back = 0; $n = 0
        if ($f.Count -ne 4 -or -not [int]::TryParse($f[0], [ref]$delete) -or
            -not [int]::TryParse($f[1], [ref]$back) -or -not [int]::TryParse($f[2], [ref]$n) -or
            $n -gt $f[3].Length) { return }
        $line = $null; $cursor = $null
        [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$cursor)
        if ($line.Substring(0, $cursor) -cne $f[3].Substring(0, $n)) {
            __lig_send 'J'
            return
        }
        $text = $f[3].Substring($n)
        $start = [Math]::Max(0, $cursor - $delete)
        [Microsoft.PowerShell.PSConsoleReadLine]::Replace($start, $cursor - $start, $text)
        [Microsoft.PowerShell.PSConsoleReadLine]::SetCursorPosition([Math]::Max(0, $start + $text.Length - $back))
    }
}
