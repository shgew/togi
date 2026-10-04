package tuner

import (
 "fmt"
 "strings"
 "testing"

 "github.com/google/go-cmp/cmp"
 "github.com/shgew/togi/internal/journal"
 "github.com/shgew/togi/internal/machine"
)

func r7Harness(t *testing.T) *harness {
 h:=newHarness(t,coreStart{phase:journal.PhaseHasRoom,offset:-30},coreStart{phase:journal.PhaseHasRoom,offset:-30},coreStart{phase:journal.PhaseHasRoom,offset:-30},coreStart{phase:journal.PhaseHasRoom,offset:-30})
 h.add(&journal.ProfileChange{To:[]int{-30,-30,-30,-30}})
 h.add(&journal.HostRanking{Ranking:[]int{0,1,2,3}})
 return h
}
func r7Fact(h *harness, pass bool, cores, profile []int, requests map[int]float64, top []int, named, stalled *int, clocks map[int]int) journal.Event {
 outcome:=journal.OutcomeFailure; if pass { outcome=journal.OutcomePass }
 return h.add(&journal.TrialCarried{Source:journal.FactSource{Session:"ruleset8",Seq:len(h.events)+1,Trial:fmt.Sprint(len(h.events))},Class:journal.TrialClass{Regime:machine.R7,Workload:machine.Workloads(machine.R7)[0].ID,Cores:cores,DurationS:120},RecordOnly:true,Condition:machine.Together,Profile:profile,Outcome:outcome,Signal:machine.ComputationError,Core:named,StalledCore:stalled,VoltageRequestsV:requests,TopRequesters:top,CCDMHz:clocks})
}
func TestR7ToleranceBoundaries(t *testing.T) {
 for _,tc:=range []struct{k,n int; move bool}{{1,1,true},{1,10,false},{2,10,true},{0,10,false},{1,4,true},{1,5,false}} {
  t.Run(fmt.Sprintf("%d/%d",tc.k,tc.n),func(t *testing.T){
   got:=binomialTail(tc.k,tc.n,.05)<.2
   if diff:=cmp.Diff(tc.move,got);diff!="" {t.Fatal(diff)}
  })
 }
}
func TestR7TolerancePreservesPassesAndDoesNotSkip(t *testing.T) {
 h:=r7Harness(t); cores:=[]int{0,1};profile:=h.s.Profile()
 for range 9 { r7Fact(h,true,cores,profile,map[int]float64{0:1.1,1:1.08},[]int{0},nil,nil,nil) }
 failure:=r7Fact(h,false,cores,profile,nil,[]int{0},nil,nil,nil)
 a,ok:=h.s.Drain();if !ok {t.Fatal("no carried tolerance before first trial")}
 p,ok:=a.Payload.(*journal.TunerDecision);if !ok || p.Decision!=journal.Tolerate {t.Fatalf("%+v",a)}
 h.decide(a)
 k:=trialClass{machine.R7,machine.Workloads(machine.R7)[0].ID,coresKey(cores),120}
 if got:=h.s.passes(k,profile,0,allEvidence);got!=9 {t.Fatalf("tolerated failure invalidated %d passes",got)}
 scheduled:=Action{Kind:RunTrial,Trial:Trial{Regime:machine.R7,Workload:k.workload,Cores:cores,DurationS:120,Condition:machine.Together,Profile:profile}}
 if diff:=cmp.Diff(RunTrial,h.s.skipKnownFailure(scheduled).Kind);diff!="" {t.Fatal(diff)}
 if len(h.s.queue)!=0 {t.Fatal("R7 queued a hunt")}
 if h.s.r7Actionable(*h.s.failureBySeq(failure.Seq)) {t.Fatal("tolerated failure actionable")}
 assertProjectionReplay(h)
}
func TestR7AttributionAndTieBackoff(t *testing.T) {
 for _,tc:=range []struct{name string;cores,top []int;named,stalled *int;want []int}{
  {"named",[]int{0,1},[]int{0},new(1),nil,[]int{1}},
  {"whole CCD",[]int{0,1},[]int{0},nil,nil,[]int{0}},
  {"all CCDs",[]int{0,1,2,3},[]int{0,2},nil,nil,[]int{0,2}},
  {"stalled CCD",[]int{0,1,2,3},[]int{0,2},nil,new(3),[]int{2}},
  {"tied CCD",[]int{0,1},[]int{0,1},nil,nil,[]int{1}},
 } {
  t.Run(tc.name,func(t *testing.T){
   h:=r7Harness(t)
   r7Fact(h,false,tc.cores,h.s.Profile(),nil,tc.top,tc.named,tc.stalled,nil)
   var moved []int
   for range 8 {a,ok:=h.s.Drain();if !ok {break};p,ok:=a.Payload.(*journal.TunerDecision);if !ok {t.Fatalf("unexpected %+v",a)};if p.Decision==journal.Backoff {moved=append(moved,p.Core);if p.ToOffset!=-29 {t.Fatalf("no-pass fallback=%d",p.ToOffset)}};h.decide(a)}
   if diff:=cmp.Diff(tc.want,moved);diff!="" {t.Fatal(diff)}
   if len(h.s.queue)!=0 || h.s.hunt!=nil {t.Fatal("multi-core R7 hunted")}
   assertProjectionReplay(h)
  })
 }
}
func TestR7VoltageTargetClockFilter(t *testing.T) {
 for _,tc:=range []struct{name string; clocks map[int]int;want int}{{"equal clock",map[int]int{0:5000},-20},{"lower clock",map[int]int{0:4900},-29},{"missing pass clock",nil,-29}} {
  t.Run(tc.name,func(t *testing.T){
   h:=r7Harness(t);cores:=[]int{0,1}
   for range h.s.n {r7Fact(h,true,cores,[]int{-20,-30,-30,-30},map[int]float64{0:1.136,1:1.08},[]int{0},nil,nil,tc.clocks)}
   r7Fact(h,false,cores,h.s.Profile(),map[int]float64{0:1.1,1:1.08},[]int{0},new(0),nil,map[int]int{0:5000})
   a,ok:=h.s.Drain();if !ok {t.Fatal("no backoff")};p,ok:=a.Payload.(*journal.TunerDecision);if !ok || p.Decision!=journal.Backoff {t.Fatalf("%+v",a)}
   if diff:=cmp.Diff(tc.want,p.ToOffset);diff!="" {t.Fatal(diff)}
   if !strings.Contains(p.Reason,"voltage-targeted") {t.Fatal(p.Reason)}
  })
 }
}
func TestR7TransitionRecordOnlyFailuresMoveBeforeTrial(t *testing.T) {
 h:=r7Harness(t)
 for range 4 {r7Fact(h,false,[]int{0,1},h.s.Profile(),nil,[]int{0},new(1),nil,nil)}
 a:=h.next();p,ok:=a.Payload.(*journal.TunerDecision);if !ok || p.Decision!=journal.Backoff || p.Core!=1 {t.Fatalf("first action %+v",a)}
}
func TestR7RequestOrderShiftsAndFallsBack(t *testing.T) {
 h:=r7Harness(t); profile:=h.s.Profile()
 r7Fact(h,true,[]int{0,1},profile,map[int]float64{0:1.08,1:1.1},[]int{1},nil,nil,nil)
 req,sources:=h.s.r7Requests(machine.Workloads(machine.R7)[0].ID,[]int{0},[]int{-20,-30,-30,-30})
 if diff:=cmp.Diff(map[int]float64{0:1.116},req);diff!="" {t.Fatal(diff)}
 if len(sources)!=1 {t.Fatal(sources)}
 top:=h.s.r7Top(machine.Workloads(machine.R7)[1].ID,[]int{0,1},profile)
 if diff:=cmp.Diff([]int{0,1},top);diff!="" {t.Fatal(diff)}
}

func TestR7LiveNamedAndMCEAttribution(t *testing.T) {
 for _,localMCE:=range []bool{false,true} {
  t.Run(fmt.Sprint(localMCE),func(t *testing.T){
   h:=r7Harness(t)
   tr:=Trial{Regime:machine.R7,Workload:machine.Workloads(machine.R7)[0].ID,Cores:[]int{0,1},DurationS:120,Condition:machine.Together}
   intent:=h.start(Action{Kind:RunTrial,Trial:tr})
   causes:=[]int{intent.Seq}
   end:=journal.TrialEnd{Trial:intent.Data.(*journal.TrialIntent).Trial,Outcome:journal.OutcomeFailure,Signal:machine.ComputationError,TopRequesters:[]int{0}}
   if localMCE { mce:=h.add(&journal.MCE{Core:1,BankType:machine.LoadStore});causes=append(causes,mce.Seq) } else {end.Core=new(1)}
   h.add(&end,causes...)
   h.decide(h.next())
   a:=h.next();move,ok:=a.Payload.(*journal.TunerDecision)
   if !ok || move.Decision!=journal.Backoff || move.Core!=1 {t.Fatalf("named core did not move: %+v",a)}
  })
 }
}
func TestR7UnattributedVoltageTarget(t *testing.T) {
 h:=r7Harness(t)
 for range h.s.n {r7Fact(h,true,[]int{0,1},[]int{-26,-30,-30,-30},map[int]float64{0:1.115,1:1.08},[]int{0},nil,nil,nil)}
 r7Fact(h,false,[]int{0,1},h.s.Profile(),map[int]float64{0:1.1,1:1.08},[]int{0},nil,nil,nil)
 a,ok:=h.s.Drain();if !ok {t.Fatal("no voltage backoff")}
 move,ok:=a.Payload.(*journal.TunerDecision)
 if !ok || move.ToOffset!=-25 {t.Fatalf("rounded voltage target: %+v",a)}
}
