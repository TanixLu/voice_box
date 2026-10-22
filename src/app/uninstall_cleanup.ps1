# Embedded in the EXE and passed to Windows PowerShell with -Command.
# Open the process handle before signaling readiness; preserve original restore paths.
$ErrorActionPreference = 'Stop'
# Read the Rust catalog snapshot before the application removes its data directory.
$texts = $env:VOICEBOX_UNINSTALL_TEXTS | ConvertFrom-Json
$script:finished = $false
$script:parentExited = $false

function Show-UninstallResult([string]$message, [bool]$failed) {
    $script:finished = $true
    $timer.Stop()
    $progress.Visible = $false
    $label.Text = $message
    $form.ControlBox = $true
    $ok.Visible = $true
    if ($failed) {
        $form.Text = $texts.UninstallIncompleteTitle
        $form.ClientSize = [System.Drawing.Size]::new(460, 300)
        $label.Size = [System.Drawing.Size]::new(420, 210)
        $label.ForeColor = [System.Drawing.Color]::Firebrick
        $ok.Location = [System.Drawing.Point]::new(350, 250)
    } else {
        $form.Text = $texts.UninstallCompleteTitle
    }
    $null = $ok.Focus()
}

try {
    Add-Type -AssemblyName System.Windows.Forms
    Add-Type -AssemblyName Microsoft.VisualBasic
    $executable = $env:VOICEBOX_UNINSTALL_EXE
    $directory = $env:VOICEBOX_UNINSTALL_DATA
    $parent = [System.Diagnostics.Process]::GetProcessById([int]$env:VOICEBOX_UNINSTALL_PROCESS_ID)
    # Force .NET to open and retain a real handle while VoiceBox is still alive.
    # WaitForExit will use this handle, so a reused PID cannot redirect the wait.
    $null = $parent.Handle
    # Keep the existing single-instance mutex alive across the process handoff.
    $instance = [System.Threading.Mutex]::OpenExisting($env:VOICEBOX_SINGLE_INSTANCE_MUTEX)
    $ready = [System.Threading.EventWaitHandle]::OpenExisting($env:VOICEBOX_UNINSTALL_READY)
    $complete = [System.Threading.EventWaitHandle]::OpenExisting($env:VOICEBOX_UNINSTALL_COMPLETE)
    $cancel = [System.Threading.EventWaitHandle]::OpenExisting($env:VOICEBOX_UNINSTALL_CANCEL)

    [System.Windows.Forms.Application]::EnableVisualStyles()
    $form = [System.Windows.Forms.Form]::new()
    $form.Text = $texts.UninstallingVoiceBox
    $form.ClientSize = [System.Drawing.Size]::new(460, 180)
    $form.StartPosition = 'CenterScreen'
    $form.FormBorderStyle = 'FixedDialog'
    $form.MaximizeBox = $false
    $form.MinimizeBox = $false
    $form.ControlBox = $false
    $form.Font = [System.Drawing.SystemFonts]::MessageBoxFont
    $form.AutoScaleMode = 'Dpi'
    $label = [System.Windows.Forms.Label]::new()
    $label.Text = $texts.UninstallProgress
    $label.Location = [System.Drawing.Point]::new(20, 20)
    $label.Size = [System.Drawing.Size]::new(420, 80)
    $progress = [System.Windows.Forms.ProgressBar]::new()
    $progress.Style = 'Marquee'
    $progress.MarqueeAnimationSpeed = 25
    $progress.Location = [System.Drawing.Point]::new(20, 110)
    $progress.Size = [System.Drawing.Size]::new(420, 18)
    $ok = [System.Windows.Forms.Button]::new()
    $ok.Text = $texts.Ok
    $ok.Location = [System.Drawing.Point]::new(350, 135)
    $ok.Size = [System.Drawing.Size]::new(90, 30)
    $ok.Visible = $false
    $ok.Add_Click({ $form.Close() })
    $form.Controls.AddRange(@($label, $progress, $ok))
    $form.Add_FormClosing({
        param($sender, $eventArgs)
        if (-not $script:finished) { $eventArgs.Cancel = $true }
    })

    # Poll without blocking the form's message loop while Rust removes the APO.
    $timer = [System.Windows.Forms.Timer]::new()
    $timer.Interval = 100
    $timer.Add_Tick({
        try {
            if ($cancel.WaitOne(0)) {
                # Rust reports the original error; leave files intact.
                $script:finished = $true
                $timer.Stop()
                $form.Close()
                return
            }
            if (-not $script:parentExited) {
                if (-not $parent.HasExited) { return }
                $parent.Dispose()
                $script:parentExited = $true
                # Allow the executable's file lock to clear before recycling.
                $timer.Interval = 1000
                return
            }
            $timer.Stop()
            if (-not $complete.WaitOne(0)) {
                throw $texts.UninstallProcessExited
            }

            # Recycle the EXE first, including when it is inside the data directory.
            $label.Text = $texts.RecyclingExecutable
            $form.Refresh()
            if (Test-Path -LiteralPath $executable -PathType Leaf) {
                [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteFile(
                    $executable, 'OnlyErrorDialogs', 'SendToRecycleBin', 'ThrowException')
            }
            if (Test-Path -LiteralPath $executable) {
                throw ($texts.RecycleExecutableIncomplete + ": " + $executable)
            }
            $label.Text = $texts.RecyclingSettings
            $form.Refresh()
            if (Test-Path -LiteralPath $directory -PathType Container) {
                [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteDirectory(
                    $directory, 'OnlyErrorDialogs', 'SendToRecycleBin', 'ThrowException')
            }
            if (Test-Path -LiteralPath $directory) {
                throw ($texts.RecycleDataIncomplete + ": " + $directory)
            }
            Show-UninstallResult $texts.UninstallComplete $false
        } catch {
            Show-UninstallResult ($texts.UninstallIncomplete + ":`r`n" + $_.Exception.Message) $true
        }
    })
    $form.Add_Shown({
        # Only let Rust close the original GUI after a progress window is visible.
        try {
            $form.Refresh()
            $null = $ready.Set()
            $timer.Start()
        } catch {
            Show-UninstallResult ($texts.UninstallWindowFailed + ":`r`n" + $_.Exception.Message) $true
        }
    })
    $null = $form.ShowDialog()
} catch {
    $null = [System.Windows.Forms.MessageBox]::Show(
        ($texts.CleanupFailed + ":`r`n" + $_.Exception.Message), 'VoiceBox', 'OK', 'Error')
    exit 1
} finally {
    if ($timer) { $timer.Dispose() }
    if ($form) { $form.Dispose() }
    if ($instance) { $instance.Dispose() }
    if ($parent) { $parent.Dispose() }
    if ($ready) { $ready.Dispose() }
    if ($complete) { $complete.Dispose() }
    if ($cancel) { $cancel.Dispose() }
}
