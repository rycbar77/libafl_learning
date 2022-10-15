use core::panic;
use std::{path::PathBuf, time::Duration};

use libafl::{
    feedback_and_fast, feedback_or, 
    prelude::{
        current_nanos, havoc_mutations, setup_restarting_mgr_std, tuple_list, AsSlice, BytesInput,
        Corpus, CrashFeedback, EventConfig, EventRestarter, ExitKind, HasTargetBytes,
        HitcountsMapObserver, InMemoryCorpus, InProcessExecutor, MaxMapFeedback, MultiMonitor,
        OnDiskCorpus, StdMapObserver, StdRand, StdScheduledMutator, TimeFeedback, TimeObserver,
        TimeoutExecutor,
    },
    schedulers::{IndexesLenTimeMinimizerScheduler, QueueScheduler},
    stages::StdMutationalStage,
    state::{HasCorpus, StdState},
    Error, Fuzzer, StdFuzzer,
};
use libafl_targets::{libfuzzer_test_one_input, EDGES_MAP, MAX_EDGES_NUM};

#[no_mangle]
fn libafl_main() -> Result<(), Error> {
    let corpus_dir = vec![PathBuf::from("./corpus")];
    let input_corpus = InMemoryCorpus::<BytesInput>::new();
    let solution_corpus = OnDiskCorpus::new(PathBuf::from("./solutions")).unwrap();

    let edges = unsafe { &mut EDGES_MAP[0..MAX_EDGES_NUM] };
    let edges_observer = HitcountsMapObserver::new(StdMapObserver::new("edges", edges));
    let time_observer = TimeObserver::new("time");

    let mut feedback = feedback_or!(
        MaxMapFeedback::new_tracking(&edges_observer, true, false),
        TimeFeedback::new_with_observer(&time_observer)
    );

    let mut objective =
        feedback_and_fast!(CrashFeedback::new(), MaxMapFeedback::new(&edges_observer));

    let monitor = MultiMonitor::new(|s| {
        println!("{}", s);
    });

    let (state, mut mgr) = match setup_restarting_mgr_std(monitor, 1337, EventConfig::AlwaysUnique)
    {
        Ok(res) => res,
        Err(err) => match err {
            Error::ShuttingDown => {
                return Ok(());
            }
            _ => {
                panic!("Failed to setup the restarting manager: {}", err);
            }
        },
    };
    let mut state = state.unwrap_or_else(|| {
        StdState::new(
            StdRand::with_seed(current_nanos()),
            input_corpus,
            solution_corpus,
            &mut feedback,
            &mut objective,
        )
        .unwrap()
    });

    let scheduler = IndexesLenTimeMinimizerScheduler::new(QueueScheduler::new());

    let mut fuzzer = StdFuzzer::new(scheduler, feedback, objective);

    let mut harness = |input: &BytesInput| {
        let target = input.target_bytes();
        let buffer = target.as_slice();
        libfuzzer_test_one_input(buffer);
        ExitKind::Ok
    };
    let inprocess_executor = InProcessExecutor::new(
        &mut harness,
        tuple_list!(edges_observer, time_observer),
        &mut fuzzer,
        &mut state,
        &mut mgr,
    )
    .unwrap();
    let timeout = Duration::from_millis(5000);

    let mut executor = TimeoutExecutor::new(inprocess_executor, timeout);

    if state.corpus().count() < 1 {
        state
            .load_initial_inputs(&mut fuzzer, &mut executor, &mut mgr, &corpus_dir)
            .unwrap_or_else(|err| {
                panic!(
                    "Failed to load initial corpus at {:?}: {:?}",
                    &corpus_dir, err
                )
            });
        println!("We imported {} inputs from disk.", state.corpus().count());
    }

    let mutator = StdScheduledMutator::new(havoc_mutations());
    let mut stages = tuple_list!(StdMutationalStage::new(mutator));

    fuzzer
        .fuzz_loop_for(&mut stages, &mut executor, &mut state, &mut mgr, 10000)
        .unwrap();
    mgr.on_restart(&mut state).unwrap();
    Ok(())
}
