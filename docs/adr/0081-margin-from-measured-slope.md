# Set the cap from measured slope

Status: accepted for the Ruleset 13 stack.

Short passing tests cannot establish a rare daily crash rate. The owner chose a margin from controlled night evidence, not an annual-rate guarantee ([#1214](https://github.com/shgew/togi/issues/1214), [#1215 Q13–Q14](https://github.com/shgew/togi/issues/1215)). [Cap S](../spec/tuner.md#cap-s-margin-from-measured-slope) owns the edge margin, incremental stock-relative budget, fallback, exact rate bound and two-sided measured-slope calculation.

A fixed steep slope and a ratchet that only accepts steeper measurements can overstate how fast failures disappear on a flatter chip. The lower confidence endpoint may therefore replace the fallback in either direction, then never increase within its validity context. The conservative maximum over classes avoids averaging away a dangerous load; uncovered classes remain explicit. Using an unsubtracted total-rate upper bound rather than an uncertain stock subtraction can cost depth, but cannot create apparent safety by subtracting noise. A nonpositive lower slope gives no extrapolation benefit.

This reverses [0038](0038-self-sufficient-cores.md)'s rejection of rate-based policy, without reviving its rejected failure-tolerance proposal: failures remain raw observations and a failed exact-profile qualification is not a pass. Frozen gate scoring and hardware observations must test the resulting policy; the model alone proves no real-machine rate.
