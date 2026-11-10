# togi: every screen

These are the owner-approved concepts for the screens to be built: `togi status`, the night screen, notifications and command replies. They describe the design in [model.md](model.md) and use only its words; they are not today's screens, which [the tuning guide](how-togi-tunes.md) and the specs describe. Every value (offsets, margins, times, dates and measurements) is an invented example.

Muted marks labels, completed phases and timestamps; good marks successful progress and nonzero gain changes; attention marks recovered crashes, pauses and first-night risk; problem marks unresolved faults and safety stops. Everything else is normal. Colour never carries meaning alone; piped status has no colour.

`togi status` uses a fixed right-aligned label column, with values and continuation lines starting in column 13. Gain uses grouped metric headings with stock, profile and change columns; numbers align under each subheader.

Night screens keep fixed regions: top Now/Problem/Next; local time, night start, elapsed time and crash count; a phase row; left current work / right units; gain rows 21-26; newest-first events at rows 29+; footer row 45 gives F10 or power-button exit during work and stays blank during return.

## togi status

### S1 Before the first night
```status
$ togi status

     State: Day, stage one: tuning not started.
   Problem: Nothing wrong.
      Next: Run togi night to begin.
     Night: would help: find CCD edges and measure gain.

   Profile: CCD 0 0 / CCD 1 0 (stock)
     Edges: not found yet
    Margin: not started
Clean time: not started
 Last move: none

      Gain: not measured yet.
```

Colours:
- muted: `State`, `Problem`, `Next`, `Night`, `Profile`, `CCD values`, `Held`, `Edges`, `Margin`, `Clean time`, `Last move`, `Gain` labels.

Rationale: The fixed labels answer state, fault and first action before the applied profile and missing results.

### S2 CCD margin narrowing
```status
$ togi status

     State: Day, stage one: CCD profile running.
   Problem: Nothing wrong.
      Next: Margin 2 after 3.5 more clean hours.
     Night: would help: find core edges and measure today's profile.

   Profile: CCD 0 -18 / CCD 1 -24
     Edges: CCD 0 -22 / CCD 1 -28
    Margin: 4
Clean time: 4.5 hours
 Last move: 10-19 13:30; margin 6 -> 4 after 8 clean hours.

      Gain: night 1, CCD 0 -12 / CCD 1 -18; today's profile unmeasured.
                           Power (W)            Temperature (°C)          Clock (MHz)     
            Shape     stock profile change    stock profile change    stock profile change
            build       230     230      0       95      95      0     4985    5120   +135
            game        121     109    -12       81      76     -5     5530    5590    +60
            idle         46      42     -4       52      49     -3     5390    5430    +40
```

Colours:
- muted: `State`; `Problem`; `Next`; `Night`; `Profile`; `Edges`; `Margin`; `Clean time`; `Last move`; `Gain`.
- good: nonzero values in the gain change columns

Rationale: The next move leads; offsets, elapsed clean time and last move follow. Gain identifies its older profile.

### S3 Crash recovered
```status
$ togi status

     State: Day, stage one: CCD profile running.
   Problem: Crash recovered; nothing to do.
      Next: Margin 3 after 7.3 more clean hours; then 16 fresh hours to margin 2.
     Night: would help: find core edges and measure today's profile.

   Profile: CCD 0 -18 / CCD 1 -24
     Edges: CCD 0 -22 / CCD 1 -28
    Margin: 4
Clean time: 0.7 hours
 Last move: 10-21 20:50 boot; margin 2 -> 4 after the 20:47 crash.

      Gain: night 1, CCD 0 -12 / CCD 1 -18; today's profile unmeasured.
                           Power (W)            Temperature (°C)          Clock (MHz)     
            Shape     stock profile change    stock profile change    stock profile change
            build       230     230      0       95      95      0     4985    5120   +135
            game        121     109    -12       81      76     -5     5530    5590    +60
            idle         46      42     -4       52      49     -3     5390    5430    +40
```

Colours:
- muted: `State`; `Problem`; `Next`; `Night`; `Profile`; `Edges`; `Margin`; `Clean time`; `Last move`; `Gain`.
- attention: `Crash recovered; nothing to do.`.
- good: nonzero values in the gain change columns

Rationale: The recovered crash is a notice, not an unresolved fault. The next two waits are ordered, not accumulated together.

### S4 A core night would help
```status
$ togi status

     State: Day, stage one: CCD profile running.
   Problem: Nothing wrong.
      Next: Margin 1 after 3.7 more clean hours.
     Night: would help: find core edges and measure today's profile.

   Profile: CCD 0 -20 / CCD 1 -26
     Edges: CCD 0 -22 / CCD 1 -28; core edges not found yet
    Margin: 2
Clean time: 4.3 hours
 Last move: 10-23 15:40; margin 3 -> 2 after 16 clean hours.

      Gain: night 1, CCD 0 -12 / CCD 1 -18; today's profile unmeasured.
                           Power (W)            Temperature (°C)          Clock (MHz)     
            Shape     stock profile change    stock profile change    stock profile change
            build       230     230      0       95      95      0     4985    5120   +135
            game        121     109    -12       81      76     -5     5530    5590    +60
            idle         46      42     -4       52      49     -3     5390    5430    +40
```

Colours:
- muted: `State`; `Problem`; `Next`; `Night`; `Profile`; `Edges`; `Margin`; `Clean time`; `Last move`; `Gain`.
- good: nonzero values in the gain change columns

Rationale: Automatic day progress stays separate from the useful optional night. CCD edges are explicitly distinguished from missing core edges.

### S5 Sixteen core offsets moving
```status
$ togi status

     State: Day, stage two: core profile running.
   Problem: Crash recovered; nothing to do.
      Next: Core margin 3 after 5.0 more clean hours.
     Night: would help: measure today's core profile.

   Profile: applied core offsets
              Core     0     1     2     3     4     5     6     7
             CCD 0   -26   -22   -30   -20   -24   -28   -19   -22
                       8     9    10    11    12    13    14    15
             CCD 1   -30   -26   -25   -30   -28   -30   -26   -28
CCD values: -19 / -25 (CCD margin 3)
     Edges: cores found on night 2
    Margin: 4 (cores)
Clean time: 11.0 hours
 Last move: 11-08 boot after a crash at core margin 3: margin 3 -> 4.

      Gain: night 2, CCD 0 -20 / CCD 1 -26; today's profile unmeasured.
                           Power (W)            Temperature (°C)          Clock (MHz)     
            Shape     stock profile change    stock profile change    stock profile change
            build       230     230      0       95      95      0     4985    5185   +200
            game        121     104    -17       81      74     -7     5530    5625    +95
            idle         46      40     -6       52      48     -4     5390    5450    +60
```

Colours:
- muted: `State`; `Problem`; `Next`; `Night`; `Profile`; `CCD values`; `Edges`; `Margin`; `Clean time`; `Last move`; `Gain`.
- attention: `Crash recovered; nothing to do.`.
- good: nonzero values in the gain change columns

Rationale: The Last move explicitly dates the boot after the crash and the completed margin change. All sixteen applied offsets have core numbers. CCD values are named without brackets or a selection-rule lesson.

### S6 Paused on the longest-used profile
```status
$ togi status

     State: Day, stage two: paused.
   Problem: Nothing wrong.
      Next: togi resume restores core margin 4; margin 3 after 2.5 more clean hours.
     Night: not needed: the held profile is measured.

   Profile: applied core offsets
              Core     0     1     2     3     4     5     6     7
             CCD 0   -20   -20   -20   -20   -20   -20   -20   -20
                       8     9    10    11    12    13    14    15
             CCD 1   -26   -26   -26   -26   -26   -26   -26   -26
      Held: most clean time, 49 hours
     Edges: cores found on night 2
    Margin: 4 (saved cores; not applied)
Clean time: 13.5 hours retained
 Last move: 11-12 08:02; togi pause applied the held profile.

      Gain: night 2, held profile.
                           Power (W)            Temperature (°C)          Clock (MHz)     
            Shape     stock profile change    stock profile change    stock profile change
            build       230     230      0       95      95      0     4985    5185   +200
            game        121     104    -17       81      74     -7     5530    5625    +95
            idle         46      40     -6       52      48     -4     5390    5450    +60
```

Colours:
- muted: `State`; `Problem`; `Next`; `Night`; `Profile`; `Held`; `Edges`; `Margin`; `Clean time`; `Last move`; `Gain`.
- attention: `paused`.
- good: nonzero values in the gain change columns

Rationale: The applied hold, its selection history and the saved counter remain distinct. Next names the restored margin and remaining wait; Night describes usefulness, not permission.

### S7 Settled and counting
```status
$ togi status

     State: Day, stage two: settled (next try more than 100 clean hours away).
   Problem: Nothing wrong.
      Next: Margin 2 after 109 more clean hours.
     Night: not needed: core edges checked; today's profile measured.

   Profile: applied core offsets
              Core     0     1     2     3     4     5     6     7
             CCD 0   -27   -23   -31   -21   -25   -29   -14   -23
                       8     9    10    11    12    13    14    15
             CCD 1   -31   -27   -25   -31   -29   -31   -27   -29
CCD values: -14 / -25 (CCD margin 3)
     Edges: cores checked on night 3
    Margin: 3 (cores)
Clean time: 19 hours
 Last move: 2027-01-15 boot after a crash at margin 2: margin 2 -> 3.
            History 12-06 01:20: core 6 failed at -19, passed -17; CCD 0 edge -22 -> -17.

      Gain: night 3, today's core profile at margin 3.
                           Power (W)            Temperature (°C)          Clock (MHz)     
            Shape     stock profile change    stock profile change    stock profile change
            build       230     230      0       95      95      0     4985    5240   +255
            game        121     101    -20       81      72     -9     5530    5650   +120
            idle         46      39     -7       52      47     -5     5390    5465    +75
```

Colours:
- muted: `State`; `Problem`; `Next`; `Night`; `Profile`; `CCD values`; `Edges`; `Margin`; `Clean time`; `Last move`; `Gain`.
- good: nonzero values in the gain change columns

Rationale: The settled parenthesis stays: bare settled reads as finished. The exact next wait confirms counting continues. The rare CCD correction is explicitly historical, once. Gain belongs to today's profile.

### S8 Watchdog unavailable
```status
$ togi status

     State: Day: CPU at stock; stage two saved.
   Problem: Watchdog device missing after a system update.
      Next: Next boot: apply the saved profile if watchdog, journal and readback checks pass.
     Night: cannot run: the watchdog is not armed.

   Profile: applied core offsets
              Core     0     1     2     3     4     5     6     7
             CCD 0     0     0     0     0     0     0     0     0
                       8     9    10    11    12    13    14    15
             CCD 1     0     0     0     0     0     0     0     0
     Edges: cores found on night 2
    Margin: 4 (saved cores; not applied)
Clean time: 12.5 hours retained; not counting at stock
 Last move: 11-11 07:12 boot: CPU at stock because the watchdog device was missing.

      Gain: night 2, CCD 0 -20 / CCD 1 -26; historical, not current stock gain.
            Saved core profile unmeasured.
                           Power (W)            Temperature (°C)          Clock (MHz)     
            Shape     stock profile change    stock profile change    stock profile change
            build       230     230      0       95      95      0     4985    5185   +200
            game        121     104    -17       81      74     -7     5530    5625    +95
            idle         46      40     -6       52      48     -4     5390    5450    +60
```

Colours:
- muted: `State`; `Problem`; `Next`; `Night`; `Profile`; `Edges`; `Margin`; `Clean time`; `Last move`; `Gain`.
- problem: `CPU at stock`; `Watchdog device missing after a system update.`.
- good: nonzero values in the gain change columns

Rationale: The fault and recovery condition lead; watchdog, journal and readback checks all condition the next application. Applied zeros and saved margin cannot be confused; the older gain is not attributed to stock.

### S9 Safety stop; day profile applied
```status
$ togi status

     State: Day, stage two: night 4 stopped at 01-10 03:14; day profile applied.
   Problem: Night readback failed on core 9: wrote -30, read back -26; nothing to do.
      Next: Margin 1 after 1.0 more clean hours.
     Night: would help: re-check core edges and measure today's profile.

   Profile: applied core offsets
              Core     0     1     2     3     4     5     6     7
             CCD 0   -28   -24   -32   -22   -26   -30   -15   -24
                       8     9    10    11    12    13    14    15
             CCD 1   -32   -28   -26   -32   -30   -32   -28   -30
CCD values: -14 / -25 (CCD margin 3)
     Edges: cores checked on night 3
    Margin: 2 (cores)
Clean time: 7.0 hours
 Last move: 01-10 03:16 boot: day profile applied; watchdog, journal and readback checks passed.

      Gain: night 3, core margin 3; today's profile not measured.
                           Power (W)            Temperature (°C)          Clock (MHz)     
            Shape     stock profile change    stock profile change    stock profile change
            build       230     230      0       95      95      0     4985    5240   +255
            game        121     101    -20       81      72     -9     5530    5650   +120
            idle         46      39     -7       52      47     -5     5390    5465    +75
```

Colours:
- muted: `State`; `Problem`; `Next`; `Night`; `Profile`; `CCD values`; `Edges`; `Margin`; `Clean time`; `Last move`; `Gain`.
- attention: `Night readback failed on core 9: wrote -30, read back -26; nothing to do.`.
- good: nonzero values in the gain change columns

Rationale: The stopped night leads, followed by the distinct checked day state and next clean-time move. Offsets and historical gain follow.

### S10 Safety stop; desktop held at stock
```status
$ togi status

     State: Day, stage two: night 4 stopped at 01-10 03:14; desktop at stock.
   Problem: Night readback failed; day readback also failed: core 9 wrote -28, read back -26.
      Next: Next boot: apply the saved profile if watchdog, journal and readback checks pass.
     Night: cannot run: readback failed.

   Profile: applied core offsets
              Core     0     1     2     3     4     5     6     7
             CCD 0     0     0     0     0     0     0     0     0
                       8     9    10    11    12    13    14    15
             CCD 1     0     0     0     0     0     0     0     0
CCD values: -14 / -25 (saved; CCD margin 3)
     Edges: cores checked on night 3
    Margin: 2 (saved cores; not applied)
Clean time: 1.8 hours retained; not counting at stock
 Last move: 01-10 03:16 boot: profile moved to stock after the day readback failed.

      Gain: night 3, core margin 3; historical, not current stock gain.
            Saved core profile not measured.
                           Power (W)            Temperature (°C)          Clock (MHz)     
            Shape     stock profile change    stock profile change    stock profile change
            build       230     230      0       95      95      0     4985    5240   +255
            game        121     101    -20       81      72     -9     5530    5650   +120
            idle         46      39     -7       52      47     -5     5390    5465    +75
```

Colours:
- muted: `State`; `Problem`; `Next`; `Night`; `Profile`; `CCD values`; `Edges`; `Margin`; `Clean time`; `Last move`; `Gain`.
- problem: `desktop at stock`; `Night readback failed; day readback also failed: core 9 wrote -28, read back -26.`.
- good: nonzero values in the gain change columns

Rationale: The night stop and separate desktop failure lead. Applied zeros, saved values and next boot condition cannot be mistaken for a running profile.

## Night screen (160 x 45)

### N1 First-night self-check
```night
    Now: Night 1, CCD edges: self-check.                                                                                                                        
Problem: Nothing wrong.                                                                                                                                         
   Next: At least one shape fails -> stock baseline; none fails -> stop: tests cannot see that failure.                                                         
                                                                                                                                                                
Time: 2026-10-16 22:47    Night started: 10-16 22:40    Elapsed: 7 minutes                                                                                      
Crashes so far: 0                                                                                                                                               
                                                                                                                                                                
Self-check: NOW           Baseline: left            Find edges: left          Soak: left                Measure: left             Return: left                  
----------------------------------------------------------------------------------------------------------------------------------------------------------------
                                                                                                                                                                
Self-check (minutes:seconds)                                                      Recorded profile that froze this desktop                                      
                                                                                                                                                                
Shape   Elapsed   Limit   Result                                                    Core     0     1     2     3     4     5     6     7                        
build      1:10   10:00   failed: wrong result                                     CCD 0   -38   -42   -35   -46   -40   -33   -44   -39                        
game       1:00   10:00   running                                                            8     9    10    11    12    13    14    15                        
idle       0:00   10:00   not started                                              CCD 1   -31   -36   -29   -41   -34   -27   -38   -32                        
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
Gain (this night): not measured yet.                                                                                                                            
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
Recent events (newest first)                                                                                                                                    
22:41  Self-check began.                                                                                                                                        
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
F10 or the power button: end the night now (CPU to stock, boot into the desktop)                                                                                
```

Colours:
- muted: `Time`; `Night started`; `Elapsed`.

Rationale: Fixed Shape, Elapsed, Limit and Result columns separate each shape's elapsed time, limit and outcome. Outcomes and the stop path are concrete; no failure glossary is added.

### N2 Finding a CCD edge
```night
    Now: Night 1, CCD edges: find edges.                                                                                                                        
Problem: Nothing wrong.                                                                                                                                         
   Next: Finish this offset's shapes; all pass -> try 2 deeper; any fails -> keep last pass, then CCD 1.                                                        
                                                                                                                                                                
Time: 2026-10-17 02:28    Night started: 10-16 22:40    Elapsed: 3 hours 48 minutes                                                                             
Crashes so far: 1                                                                                                                                               
                                                                                                                                                                
Self-check: done          Baseline: done            Find edges: NOW           Soak: left                Measure: left             Return: left                  
----------------------------------------------------------------------------------------------------------------------------------------------------------------
                                                                                                                                                                
Current probe: CCD 0, cores 0-7 loaded                                            CCD edges: 0 of 2 found; 2 left.                                              
                                                                                                                                                                
Offset ladder                                                                     CCD 0  left                                                                   
  Passed:       -18   -20                                                         CCD 1  left                                                                   
  Now trying:   -22                                                                                                                                             
                                                                                  All cores together: passed -2 through -16, step 2                             
Shapes: build passed; game running; idle left.                                                                                                                  
Game probe (minutes:seconds): 2:10 of 5:00                                        Other cores held at their day value: CCD 1 -6.                                
                                                                                                                                                                
                                                                                                                                                                
Gain (this night): not measured yet.                                                                                                                            
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
Recent events (newest first)                                                                                                                                    
01:50  Individual CCD probing began.                                                                                                                            
01:47  All cores together failed at -18: idle, wrong result on core 12.                                                                                         
22:57  Tuning boot returned after the self-check hang.                                                                                                          
22:55  Watchdog reset the machine.                                                                                                                              
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
F10 or the power button: end the night now (CPU to stock, boot into the desktop)                                                                                
```

Colours:
- muted: `Time`; `Night started`; `Elapsed`.
- good: `-18   -20`; `build passed`.

Rationale: The ladder alone owns the tested offset; the right column counts found and remaining CCD edges, marks both unfinished edges left, and shows other cores held at day values.

### N3 Finding a core edge
```night
    Now: Night 2, core edges: find edges.                                                                                                                       
Problem: Nothing wrong.                                                                                                                                         
   Next: Finish this offset's shapes; all pass -> try 2 deeper; any fails -> keep last pass, then core 12.                                                      
                                                                                                                                                                
Time: 2026-10-25 06:40    Night started: 10-24 22:30    Elapsed: 8 hours 10 minutes                                                                             
Crashes so far: 2                                                                                                                                               
                                                                                                                                                                
Self-check: night 1       Baseline: night 1         Find edges: NOW           Soak: left                Measure: left             Return: left                  
----------------------------------------------------------------------------------------------------------------------------------------------------------------
                                                                                                                                                                
Current probe: core 11 loaded                                                     Core edges: 11 of 16 found; 5 left.                                           
                                                                                                                                                                
Offset ladder                                                                       Core     0     1     2     3     4     5     6     7                        
  Passed:       -26 -28 -30 -32 -34 -36                                            CCD 0   -30   -26   -34   -24   -28   -32   -22   -26                        
  Now trying:   -38                                                                          8     9    10    11    12    13    14    15                        
                                                                                   CCD 1   -34   -30   -28  left  left  left  left  left                        
Shapes: build running; game and idle left.                                                                                                                      
Build probe (minutes:seconds): 1:10 of 3:30                                       Other cores held at their day value:                                          
                                                                                  CCD 0 -20, CCD 1 -26.                                                         
                                                                                                                                                                
Gain (this night): not measured yet.                                                                                                                            
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
Recent events (newest first)                                                                                                                                    
03:55  Search resumed.                                                                                                                                          
03:51  Machine reset probing core 5 at -34; edge -32.                                                                                                           
00:41  Machine reset probing core 2 at -36; edge -34.                                                                                                           
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
F10 or the power button: end the night now (CPU to stock, boot into the desktop)                                                                                
```

Colours:
- muted: `Time`; `Night started`; `Elapsed`.
- good: `-26 -28 -30 -32 -34 -36`.

Rationale: Two preceding resets match the crash count. The edge progress line counts found and remaining cores; every unfinished edge says left, while the current probe stays on the left.

### N4 Whole-profile soak
```night
    Now: Night 2, core edges: soak.                                                                                                                             
Problem: Nothing wrong.                                                                                                                                         
   Next: Finish game and idle, then measure the day profile; a failure backs off the affected edges.                                                            
                                                                                                                                                                
Time: 2026-10-25 11:20    Night started: 10-24 22:30    Elapsed: 12 hours 50 minutes                                                                            
Crashes so far: 4                                                                                                                                               
                                                                                                                                                                
Self-check: night 1       Baseline: night 1         Find edges: done          Soak: NOW                 Measure: left             Return: left                  
----------------------------------------------------------------------------------------------------------------------------------------------------------------
                                                                                                                                                                
Current shape: game                                                               Applied core-edge profile                                                     
                                                                                                                                                                
Shape elapsed: 20 minutes                                                           Core     0     1     2     3     4     5     6     7                        
Soak elapsed: 40 minutes                                                           CCD 0   -30   -26   -34   -24   -28   -32   -22   -26                        
Shapes: build passed; idle left.                                                             8     9    10    11    12    13    14    15                        
                                                                                   CCD 1   -34   -30   -28   -36   -32   -34   -30   -32                        
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
Gain (this night): not measured yet.                                                                                                                            
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
Recent events (newest first)                                                                                                                                    
11:00  Build passed; game began.                                                                                                                                
10:40  Soak began.                                                                                                                                              
09:05  Machine reset probing core 13 at -36; edge -34.                                                                                                          
07:22  Machine reset probing core 11 at -38; edge -36.                                                                                                          
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
F10 or the power button: end the night now (CPU to stock, boot into the desktop)                                                                                
```

Colours:
- muted: `Time`; `Night started`; `Elapsed`.
- good: `build passed`; `Build passed`.

Rationale: Current-shape elapsed time and whole-soak elapsed time are distinct; neither invents a total. Applied edges need no tutorial.

### N5 Recovered and probing again
```night
    Now: Night 2, core edges: find edges.                                                                                                                       
Problem: Crash recovered; nothing to do.                                                                                                                        
   Next: Finish this offset's shapes; all pass -> try -22; any fails -> step shallower until one passes, and CCD 0's edge moves back to it.                     
                                                                                                                                                                
Time: 2026-10-25 03:55    Night started: 10-24 22:30    Elapsed: 5 hours 25 minutes                                                                             
Crashes so far: 2                                                                                                                                               
                                                                                                                                                                
Self-check: night 1       Baseline: night 1         Find edges: NOW           Soak: left                Measure: left             Return: left                  
----------------------------------------------------------------------------------------------------------------------------------------------------------------
                                                                                                                                                                
Current probe: core 6 loaded                                                      Core edges: 6 of 16 found; 10 left.                                           
                                                                                                                                                                
Offset ladder                                                                       Core     0     1     2     3     4     5     6     7                        
  Passed:       none yet                                                           CCD 0   -30   -26   -34   -24   -28   -32  left  left                        
  Now trying:   -20                                                                          8     9    10    11    12    13    14    15                        
                                                                                   CCD 1  left  left  left  left  left  left  left  left                        
Shapes: build running; game and idle left.                                                                                                                      
Build probe (minutes:seconds): 0:00 of 3:30                                       Other cores held at their day value:                                          
                                                                                  CCD 0 -20, CCD 1 -26.                                                         
                                                                                                                                                                
Gain (this night): not measured yet.                                                                                                                            
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
Recent events (newest first)                                                                                                                                    
03:51  Machine reset probing core 5 at -34; edge -32.                                                                                                           
00:41  Machine reset probing core 2 at -36; edge -34.                                                                                                           
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
F10 or the power button: end the night now (CPU to stock, boot into the desktop)                                                                                
```

Colours:
- muted: `Time`; `Night started`; `Elapsed`.
- attention: `Crash recovered; nothing to do.`.

Rationale: Recovery is reported once; the current probe is already running. The event gives the prior failed offset without repeating the live one. Edge progress counts found and remaining cores, and every unfinished edge says left.

### N6 Returning with measured gain
```night
    Now: Night 2, core edges: work done; CPU at stock; booting into the desktop now.                                                                            
Problem: Nothing wrong.                                                                                                                                         
   Next: The desktop applies the day profile if its own checks pass. Core day moves not started.                                                                
                                                                                                                                                                
Time: 2026-10-25 13:48    Night started: 10-24 22:30    Elapsed: 15 hours 18 minutes                                                                            
Crashes so far: 4                                                                                                                                               
                                                                                                                                                                
Self-check: night 1       Baseline: night 1         Find edges: done          Soak: done                Measure: done             Return: NOW                   
----------------------------------------------------------------------------------------------------------------------------------------------------------------
                                                                                                                                                                
Day profile measured                                                              Core edges found                                                              
                                                                                                                                                                
CCD 0, cores 0-7:   -20                                                             Core     0     1     2     3     4     5     6     7                        
CCD 1, cores 8-15:  -26                                                            CCD 0   -30   -26   -34   -24   -28   -32   -22   -26                        
                                                                                             8     9    10    11    12    13    14    15                        
                                                                                   CCD 1   -34   -30   -28   -34   -32   -34   -30   -32                        
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
Gain: night 2, day profile above; stock baseline from night 1.                                                                                                  
               Power (W)            Temperature (°C)          Clock (MHz)                                                                                       
Shape     stock profile change    stock profile change    stock profile change                                                                                  
build       230     230      0       95      95      0     4985    5185   +200                                                                                  
game        121     104    -17       81      74     -7     5530    5625    +95                                                                                  
idle         46      40     -6       52      48     -4     5390    5450    +60                                                                                  
                                                                                                                                                                
                                                                                                                                                                
Recent events (newest first)                                                                                                                                    
13:45  Measurement finished.                                                                                                                                    
12:50  Soak finished.                                                                                                                                           
11:58  Core 11 returned a wrong result; its edge backed off by 2.                                                                                               
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
```

Colours:
- muted: `Time`; `Night started`; `Elapsed`.
- good: nonzero values in the gain change columns

Rationale: The core-edge table stays in the right-hand region used during probing and soak; the measured day profile occupies the left-hand work region with equal-width labels and right-aligned offsets. Gain fits above the unchanged events region. The blank footer avoids repeating the stock-and-desktop transition already stated at the top.

### N7 Readback failure stopped the night
```night
    Now: Night 4, core edges: stopped; CPU at stock; booting into the desktop now.                                                                              
Problem: Readback failed on core 9: wrote -30, read back -26.                                                                                                   
   Next: The desktop applies the day profile if its own checks pass.                                                                                            
                                                                                                                                                                
Time: 2027-01-10 03:14    Night started: 01-09 22:30    Elapsed: 4 hours 44 minutes                                                                             
Crashes so far: 1                                                                                                                                               
                                                                                                                                                                
Self-check: night 1       Baseline: night 1         Find edges: STOPPED       Soak: not run             Measure: not run          Return: NOW                   
----------------------------------------------------------------------------------------------------------------------------------------------------------------
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
Gain (this night): not measured; the night stopped.                                                                                                             
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
Recent events (newest first)                                                                                                                                    
03:14  Night stopped.                                                                                                                                           
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
                                                                                                                                                                
```

Colours:
- muted: `Time`; `Night started`; `Elapsed`.
- problem: `stopped; CPU at stock`; `Readback failed on core 9: wrote -30, read back -26.`.

Rationale: The stopped phase and mismatch explain the immediate return; the fixed gain region reports no measurement, the blank footer avoids repeating the top's stock-and-desktop transition, and the problem colour targets the exact displayed stop span. No unfinished-edge policy is invented.

## Notifications

### M1 Profile moved deeper
```notify
togi: Day profile moved deeper
CCD 0 -18 / CCD 1 -24: margin 6 -> 4 after 8 clean hours; next: margin 2 after 8 more clean hours.
```

Colours:
- good: `profile moved deeper`.

Rationale: The title says what moved and whether anything is wrong. The body gives the applied profile, cause and next wait.

### M2 Crash recovered and backed off
```notify
togi: Day profile backed off after a crash
CCD 0 -18 / CCD 1 -24; margin 4; next: margin 3 after 8 clean hours.
```

Colours:
- attention: `profile backed off after a crash`.

Rationale: The changed profile and handled crash are clear. The fresh boot's next wait is explicit, with no approval requested.

### M3 First night moved the day profile
```notify
togi: night 1 done; day profile moved from stock
CCD 0 -12 / CCD 1 -18; margin 10; next: margin 8 after 8 clean hours.
```

Colours:
- good: `day profile moved from stock`.

Rationale: This notification is caused by night 1's actual profile move. Naming no problem says nothing is wrong; the next condition is the first day move, not merely automatic tuning.

### M4 Settled after a back-off
```notify
togi: Core profile backed off after a crash (settled)
Cores -31 to -14; margin 3; next: margin 2 after 128 clean hours.
```

Colours:
- attention: `Core profile backed off after a crash`.

Rationale: The brief title names the back-off. The profile range and exact long retry wait replace a definition of settled; no label-only notification is implied.

### M5 Day profile moved to stock
```notify
togi: CPU at stock; day readback failed
Core 9 wrote -28, read back -26. The saved day profile returns at the next boot if its checks pass.
```

Colours:
- problem: `CPU at stock; day readback failed`.

Rationale: The title distinguishes the CPU's stock state from the failed day readback; the body explicitly keeps the saved day profile, identifies the mismatch and names the next boot attempt.

## Command replies

### R1 Starting the first night
```reply
$ togi night
Unsaved work will be lost; night tests can crash and reset the machine.
Self-check starts after the reboot. The desktop comes back when the night's work is done.
Start the first night now? [y/N]
```

Colours:
- attention: `Unsaved work will be lost; night tests can crash and reset the machine.`

Rationale: The risk precedes the first phase and normal return. The only question authorises the first reboot.

### R2 Pause holds the longest-used profile
```reply
$ togi pause
Day, stage two: paused.
Holding CCD 0 -20 / CCD 1 -26; most clean time, 49 hours.
togi resume restores the prior core profile, margin 4 and 13.5 retained clean hours.
```

Colours:
- attention: `paused`.

Rationale: The reply names the changed hold and why it was chosen. The resume command identifies the different saved moving state without teaching pause.

### R3 Resume restores and continues
```reply
$ togi resume
Day, stage two: prior core profile restored; core margin 4.
Retained 13.5 clean hours; next: margin 3 after 2.5 more clean hours.
```

Colours:
- muted: `next:`.

Rationale: The restored state, retained progress and single next condition fit two lines; no clean-time definition or repeated profile table is needed.

### R4 Starting a later night
```reply
$ togi night
Rebooting into night 3 now: re-check core edges.
```

Colours:
- muted: `night 3`.

Rationale: The later command gives its immediate effect, work and explicit nothing-wrong answer without another question or risk recital.
