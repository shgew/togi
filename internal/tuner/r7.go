package tuner

import (
 "fmt"
 "math"
 "slices"
 "strconv"
 "strings"

 "github.com/shgew/togi/internal/journal"
 "github.com/shgew/togi/internal/machine"
 "github.com/shgew/togi/internal/requests"
)

func (s *State) classCores(k trialClass) []int {
 if t, ok := s.classTargets[k.cores]; ok { return t.cores }
 var out []int
 for _, text := range strings.Fields(strings.Trim(k.cores, "[]")) { id, err := strconv.Atoi(text); if err == nil { out = append(out, id) } }
 return out
}
func (s *State) multiR7(k trialClass) bool { return k.regime == machine.R7 && len(s.classCores(k)) > 1 }

func (s *State) r7Requests(workload string, cores []int, profile []int) (map[int]float64, []int) {
 var exact *entry
 full := map[int]*entry{}
 for i := range s.r7Measurements {
  e := &s.r7Measurements[i]
  if e.class.workload != workload || len(e.requests) == 0 { continue }
  if slices.Equal(e.cores, cores) && (exact == nil || e.seq > exact.seq) { exact = e }
  for _, ccd := range s.ccdIDs(cores) {
   var part []int
   for _, id := range s.ids() { if s.ccd[id] == ccd { part = append(part,id) } }
   if slices.Equal(e.cores,part) && (full[ccd] == nil || e.seq > full[ccd].seq) { full[ccd] = e }
  }
 }
 out := make(map[int]float64,len(cores))
 var sources []int
 for _,id := range cores {
  source := exact
  if source == nil { source = full[s.ccd[id]] }
  if source != nil {
   if v,ok := source.requests[id]; ok {
    i:=s.index(id); out[id]=v+requests.VoltsPerCount*float64(profile[i]-source.profile[i])
    if !slices.Contains(sources,source.seq) { sources=append(sources,source.seq) }
    continue
   }
  }
  out[id]=requests.VoltsPerCount*float64(profile[s.index(id)])
 }
 slices.Sort(sources)
 return out,sources
}
func (s *State) r7Top(workload string, cores []int, profile []int) []int {
 req,_ := s.r7Requests(workload,cores,profile)
 var top []int
 for _,ccd := range s.ccdIDs(cores) {
  part:=map[int]float64{}
  for _,id:=range cores { if s.ccd[id]==ccd { part[id]=req[id] } }
  if groups:=requests.Groups(part);len(groups)>0 { top=append(top,groups[0]...) }
 }
 slices.Sort(top)
 return top
}
func (s *State) ccdIDs(cores []int) []int {
 var ids []int
 for _, id := range cores { if !slices.Contains(ids, s.ccd[id]) { ids = append(ids, s.ccd[id]) } }
 slices.Sort(ids)
 return ids
}
func (s *State) entryTop(e entry) []int {
 if len(e.top) > 0 { return e.top }
 if len(e.requests) > 0 {
  var top []int
  for _, ccd := range s.ccdIDs(e.cores) {
   req := map[int]float64{}
   for _, id := range e.cores { if s.ccd[id] == ccd { req[id] = e.requests[id] } }
   if groups := requests.Groups(req); len(groups) > 0 { top = append(top, groups[0]...) }
  }
  slices.Sort(top); return top
 }
 return s.r7Top(e.class.workload, e.cores, e.profile)
}
func (s *State) failureTargets(e entry) []int {
 if e.named != nil { return []int{*e.named} }
 top := s.entryTop(e)
 if e.stalled != nil { top = slices.DeleteFunc(slices.Clone(top), func(id int) bool { return s.ccd[id] != s.ccd[*e.stalled] }) }
 return top
}
func binomialTail(k, n int, rate float64) float64 {
 if k <= 0 { return 1 }
 sum := 0.0
 for i := k; i <= n; i++ {
  a, _ := math.Lgamma(float64(n+1)); b, _ := math.Lgamma(float64(i+1)); c, _ := math.Lgamma(float64(n-i+1))
  sum += math.Exp(a-b-c+float64(i)*math.Log(rate)+float64(n-i)*math.Log1p(-rate))
 }
 return min(1, sum)
}
func (s *State) r7Counts(c *core, k trialClass) (int, int, []int) {
 failures, passes := 0, 0
 var causes []int
 for class, entries := range s.ledger {
  if class.regime != machine.R7 || class.workload != k.workload || class.duration != k.duration { continue }
  for _, e := range entries {
   if !slices.Contains(e.cores, c.id) || len(e.profile) != len(s.cores) { continue }
   offset := e.profile[s.index(c.id)]
   if !e.pass && offset >= c.offset && slices.Contains(s.failureTargets(e), c.id) { failures++; causes = append(causes, e.seq) }
   if e.pass && offset <= c.offset && slices.Contains(s.entryTop(e), c.id) { passes++; causes = append(causes, e.seq) }
  }
 }
 slices.Sort(causes)
 return failures, failures+passes, causes
}
func (s *State) consumeR7(ev journal.Event, id int, moved bool) {
 for _, seq := range ev.Cause {
  f := s.failureBySeq(seq)
  if f == nil || !s.multiR7(f.class) { continue }
  if s.r7Handled == nil { s.r7Handled = map[int]map[int]bool{} }
  if s.r7Handled[f.seq] == nil { s.r7Handled[f.seq] = map[int]bool{} }
  s.r7Handled[f.seq][id] = true
  if moved {
   entries := s.ledger[f.class]
   for i := range entries { if s.sameR7Failure(entries[i].seq,f.seq) {
    entries[i].actionable = true
    if entries[i].named == nil { for _,other:=range s.failureTargets(entries[i]) { if s.ccd[other]==s.ccd[id] { s.r7Handled[f.seq][other]=true } } }
   } }
   s.ledger[f.class] = entries
  }
  break
 }
}
func (s *State) r7Decision() (Action, bool) {
 a,ok:=s.r7PendingDecision()
 if p,isDecision:=a.Payload.(*journal.TunerDecision);isDecision && len(a.Cause)>0 {
  if f:=s.failureBySeq(a.Cause[0]);f!=nil {
   _,sources:=s.r7Requests(f.class.workload,s.classCores(f.class),f.profile)
   a.Cause=append(a.Cause,sources...)
   if len(sources)==0 && !strings.Contains(p.Reason,"fell back to offsets") { p.Reason+="; request order fell back to offsets" }
  }
 }
 var unique []int
 for _,seq:=range a.Cause { if !slices.Contains(unique,seq) { unique=append(unique,seq) } }
 a.Cause=unique
 return a,ok
}
func (s *State) r7PendingDecision() (Action, bool) {
 for _, f := range s.pendingFailures {
  if !s.multiR7(f.class) { continue }
  var failed *entry
  for i := range s.ledger[f.class] { e := &s.ledger[f.class][i]; if s.sameR7Failure(e.seq,f.seq) { failed = e; break } }
  if failed == nil { continue }
  if failed.named==nil && f.failure.Core!=nil { failed.named=f.failure.Core }
  targets := s.failureTargets(*failed)
  for _, id := range targets {
   if s.r7Handled[f.seq][id] { continue }
   c := s.core(id); if c == nil { continue }
   k, n, causes := s.r7Counts(c, f.class)
   rate, alpha := s.evidence.FailureRate, s.evidence.Significance
   if rate == 0 { rate = .05 }; if alpha == 0 { alpha = .2 }
   cause := append([]int{f.seq}, causes...)
   if binomialTail(k,n,rate) >= alpha {
    return Action{Kind: Decide, Payload: &journal.TunerDecision{Core:id, Phase:journal.PhaseChecking, Decision:journal.Tolerate, FromOffset:c.offset, ToOffset:c.offset, Pass:c.pass, FailurePoint:c.fail, Reason:fmt.Sprintf("tolerated R7 failure #%d: core %02d has %d failures in %d starts; binomial tail %.6g >= significance %.6g%s", f.seq,id,k,n,binomialTail(k,n,rate),alpha,s.carriedReason(cause))}, Cause:cause}, true
   }
   // An unattributed tie moves only its lowest-preferred member on each CCD.
   if failed.named == nil {
    chosen := id
    tied:=0
    for _,other:=range targets { if s.ccd[other]==s.ccd[id] { tied++ } }
    if tied>1 && s.rankingSeq==0 { return Action{Kind:ReadRanking},true }
    if tied>1 { cause=append(cause,s.rankingSeq) }
    for _, other := range targets {
     if s.ccd[other] != s.ccd[id] || s.r7Handled[f.seq][other] { continue }
     otherCore:=s.core(other); if otherCore==nil { continue }
     otherK,otherN,_:=s.r7Counts(otherCore,f.class)
     if binomialTail(otherK,otherN,rate)<alpha && s.lowerPreferred(other,chosen) { chosen=other }
    }
    if id != chosen { continue }
   }
   if failed.profile[s.index(id)] == 0 { return Action{Kind:Decide, Payload:failedAtZero(id), Cause:[]int{f.seq}},true }
   if s.round != nil { return Action{Kind:Decide, Payload:&journal.DeepeningRound{Round:s.round.start.Round,Event:journal.LapEnd,Reason:fmt.Sprintf("R7 failure #%d exceeds tolerance",f.seq)},Cause:[]int{f.seq}},true }
   req, sources := s.r7Requests(f.class.workload,failed.cores,failed.profile)
   if len(failed.requests)>0 { req = failed.requests; sources=[]int{failed.seq} }
   target, passSeqs := s.r7VoltageTarget(*failed,id,req[id])
   counts := 1
   reason := fmt.Sprintf("voltage-targeted R7 backoff after failure #%d: core %02d request %.6f V; no qualifying pass, one count",f.seq,id,req[id])
   if len(passSeqs)>0 { counts=requests.Counts(req[id],target); reason=fmt.Sprintf("voltage-targeted R7 backoff after failure #%d: core %02d request %.6f V to %.6f V, %d counts",f.seq,id,req[id],target,counts) }
   if len(sources)==0 { reason += "; request order fell back to offsets" }
   cause=append(cause,sources...); cause=append(cause,passSeqs...)
   fail:=failed.profile[s.index(id)]; if c.fail!=nil { fail=max(fail,*c.fail) }
   pass,_:=keepPass(c.pass,fail)
   to:=min(0,max(c.offset,failed.profile[s.index(id)]+counts,fail+1))
   return Action{Kind:Decide,Payload:&journal.TunerDecision{Core:id,Phase:journal.PhaseChecking,Decision:journal.Backoff,FromOffset:c.offset,ToOffset:to,Pass:pass,FailurePoint:new(fail),Reason:reason+s.carriedReason(cause)},Cause:cause},true
  }
 }
 return Action{},false
}
func (s *State) lowerPreferred(a,b int) bool {
 ra,rb:=slices.Index(s.ranking,a),slices.Index(s.ranking,b)
 if ra!=rb { return ra>rb }; return a>b
}
func (s *State) r7VoltageTarget(f entry,id int,request float64) (float64,[]int) {
 type candidate struct { voltage float64; seqs []int }
 groups:=map[string]*candidate{}
 for class,entries:=range s.ledger {
  if class.regime!=machine.R7 || class.workload!=f.class.workload { continue }
  for _,e:=range entries {
   if !e.pass || len(e.requests)==0 || !slices.Contains(e.cores,id) { continue }
   if f.named==nil && !slices.Equal(e.cores,f.cores) { continue }
   if f.named!=nil { if clock,ok:=f.clocks[s.ccd[id]]; ok && e.clocks[s.ccd[id]]<clock { continue } }
   req:=map[int]float64{}
   for _,core:=range e.cores { if f.named!=nil || s.ccd[core]==s.ccd[id] { req[core]=e.requests[core] } }
   voltage,ok:=requests.Top(req); if !ok || f.named==nil && voltage<=request { continue }
   key:=fmt.Sprintf("%v/%s",e.profile,class.cores)
   group:=groups[key]; if group==nil { group=&candidate{voltage:voltage};groups[key]=group }; group.voltage=min(group.voltage,voltage);group.seqs=append(group.seqs,e.seq)
  }
 }
 best:=math.Inf(1);var seqs []int
 for _,g:=range groups {
  slices.Sort(g.seqs)
  if len(g.seqs)>=s.n && (g.voltage<best || g.voltage==best && (len(seqs)==0 || g.seqs[0]<seqs[0])) {
   best=g.voltage;seqs=slices.Clone(g.seqs[:s.n])
  }
 }
 return best,seqs
}

func (s *State) sameR7Failure(a,b int) bool {
 if a==b { return true }
 ai,aok:=s.failureIndex[a];bi,bok:=s.failureIndex[b]
 return aok && bok && ai==bi
}
func (s *State) recordR7Measurement(seq int,p *journal.TrialIntent,end *journal.TrialEnd) {
 if p.Regime!=machine.R7 || len(end.VoltageRequestsV)==0 { return }
 s.r7Measurements=append(s.r7Measurements,entry{seq:seq,class:classOf(p),cores:slices.Clone(p.Cores),profile:slices.Clone(p.Profile),requests:end.VoltageRequestsV,top:end.TopRequesters,clocks:end.CCDMHz})
}

func (s *State) r7Actionable(f pendingFailure) bool {
 for _,e:=range s.ledger[f.class] { if s.sameR7Failure(e.seq,f.seq) { return e.actionable } }
 return false
}
