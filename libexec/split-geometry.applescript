on rzPaneFrames(expectedCount)
    repeat 8 times
        set paneFrames to {}
        try
            tell application "System Events" to tell process "Ghostty"
                set allElements to entire contents of front window
                repeat with currentElement in allElements
                    try
                        if (role of currentElement as text) is "AXTextArea" then
                            set panePosition to position of currentElement
                            set paneSize to size of currentElement
                            if item 1 of paneSize > 0 and item 2 of paneSize > 0 then
                                set end of paneFrames to {item 1 of panePosition, item 2 of panePosition, item 1 of paneSize, item 2 of paneSize}
                            end if
                        end if
                    end try
                end repeat
            end tell
        end try
        if (count of paneFrames) is expectedCount then return paneFrames
        delay 0.02
    end repeat
    return {}
end rzPaneFrames

on rzCapturePaneFrames(targetTab, originalTab, expectedCount)
    set paneFrames to {}
    try
        tell application "Ghostty" to select tab targetTab
        set paneFrames to my rzPaneFrames(expectedCount)
    end try
    try
        tell application "Ghostty" to select tab originalTab
    end try
    return paneFrames
end rzCapturePaneFrames

on rzResizeSplit(firstTerminal, secondTerminal, targetTab, firstSize, totalSize, splitDirection)
    try
        tell application "Ghostty"
            select tab targetTab
            set firstId to id of firstTerminal
            set secondId to id of secondTerminal
            set terminalIds to id of every terminal of targetTab
        end tell
        set firstIndex to 0
        set secondIndex to 0
        repeat with terminalIndex from 1 to count of terminalIds
            if item terminalIndex of terminalIds is firstId then set firstIndex to terminalIndex
            if item terminalIndex of terminalIds is secondId then set secondIndex to terminalIndex
        end repeat
        if firstIndex is 0 or secondIndex is 0 then return false
        repeat 5 times
            set paneFrames to my rzPaneFrames(count of terminalIds)
            if (count of paneFrames) is not (count of terminalIds) then return false
            set firstFrame to item firstIndex of paneFrames
            set secondFrame to item secondIndex of paneFrames
            if splitDirection is "right" then
                set currentSize to item 3 of firstFrame
                set currentTotal to (item 1 of secondFrame) + (item 3 of secondFrame) - (item 1 of firstFrame)
            else
                set currentSize to item 4 of firstFrame
                set currentTotal to (item 2 of secondFrame) + (item 4 of secondFrame) - (item 2 of firstFrame)
            end if
            set targetSize to round (firstSize * currentTotal / totalSize) rounding as taught in school
            set difference to targetSize - currentSize
            if difference is 0 then return true
            set resizeDirection to splitDirection
            if difference < 0 then
                set difference to -difference
                if splitDirection is "right" then
                    set resizeDirection to "left"
                else
                    set resizeDirection to "up"
                end if
            end if
            tell application "Ghostty" to perform action ("resize_split:" & resizeDirection & "," & difference) on firstTerminal
            delay 0.04
        end repeat
    end try
    return false
end rzResizeSplit
