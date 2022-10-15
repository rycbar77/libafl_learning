use libafl::bolts::shmem::{ShMem, ShMemProvider, StdShMemProvider};
use libafl::corpus::{InMemoryCorpus, OnDiskCorpus};
use libafl::inputs::BytesInput;
use libafl::observers::TimeObserver;
use libafl::prelude::{
    current_nanos, havoc_mutations, setup_restarting_mgr_std, tuple_list, AsMutSlice,
    ConstMapObserver, Corpus, EventConfig, EventRestarter, ForkserverExecutor,
    HitcountsMapObserver, InProcessExecutor, MaxMapFeedback, Monitor, MultiMonitor,
    SimpleEventManager, SimpleMonitor, StdRand, StdScheduledMutator, TimeFeedback, TimeoutFeedback,
    TimeoutForkserverExecutor, Tokens,
};
use libafl::schedulers::{IndexesLenTimeMinimizerScheduler, QueueScheduler};
use libafl::stages::StdMutationalStage;
use libafl::state::{HasCorpus, HasMetadata, StdState};
use libafl::Error;
use libafl::{feedback_and_fast, feedback_or, Fuzzer, StdFuzzer};
use nix::sys::signal::Signal;
use std::path::PathBuf;
use std::time::Duration;

pub fn main() {
    let corpus_dirs = vec![PathBuf::from("./corpus")];
    let input_corpus = InMemoryCorpus::<BytesInput>::new();
    let timeouts_corpus =
        OnDiskCorpus::new(PathBuf::from("./timeouts")).expect("Could not create timeouts corpus");

    let time_observer = TimeObserver::new("time");
    const MAP_SIZE: usize = 65536;
    let mut shmem_provider = StdShMemProvider::new().unwrap();
    let mut shmem = shmem_provider.new_shmem(MAP_SIZE).unwrap();
    shmem
        .write_to_env("__AFL_SHM_ID")
        .expect("couldn't write shared memory ID");

    let mut shmem_map = shmem.as_mut_slice();
    let edges_observer = HitcountsMapObserver::new(ConstMapObserver::<_, MAP_SIZE>::new(
        "shared_mem",
        &mut shmem_map,
    ));

    let mut feedback = feedback_or!(
        MaxMapFeedback::new_tracking(&edges_observer, true, false),
        TimeFeedback::new_with_observer(&time_observer)
    );
    let mut objective = feedback_and_fast!(
        TimeoutFeedback::new(),
        MaxMapFeedback::<_, _, _, u8>::new(&edges_observer)
    );

    // let monitor = SimpleMonitor::new(|s| {
    //     println!("{}", s);
    // });
    // let mut mgr = SimpleEventManager::new(monitor);

    let monitor = MultiMonitor::new(|s| {
        println!("{}", s);
    });

    let (state, mut mgr) = match setup_restarting_mgr_std(monitor, 1337, EventConfig::AlwaysUnique)
    {
        Ok(res) => res,
        Err(err) => match err {
            Error::ShuttingDown => {
                return ();
            }
            _ => {
                panic!("Failed to setup the restarting manager:{}", err);
            }
        },
    };

    let mut state = state.unwrap_or_else(|| {
        StdState::new(
            StdRand::with_seed(current_nanos()),
            input_corpus,
            timeouts_corpus,
            &mut feedback,
            &mut objective,
        )
        .unwrap()
    });

    // let mut state = StdState::new(
    //     StdRand::with_seed(current_nanos()),
    //     input_corpus,
    //     timeouts_corpus,
    //     &mut feedback,
    //     &mut objective,
    // )
    // .unwrap();

    let scheduler = IndexesLenTimeMinimizerScheduler::new(QueueScheduler::new());
    let mut fuzzer = StdFuzzer::new(scheduler, feedback, objective);

    let mut tokens = Tokens::new();

    let forkserver = ForkserverExecutor::builder()
        .program("./xpdf/install/bin/pdftotext".to_string())
        .debug_child(true)
        .shmem_provider(&mut shmem_provider)
        .autotokens(&mut tokens)
        // .parse_afl_cmdline(&[String::from("@@")])
        .build(tuple_list!(time_observer, edges_observer))
        .unwrap();

    let mut executor = TimeoutForkserverExecutor::with_signal(
        forkserver,
        Duration::from_millis(5000),
        "SIGKILL".parse::<Signal>().unwrap(),
    )
    .expect("failed to create the executor");

    if state.corpus().count() < 1 {
        state
            .load_initial_inputs(&mut fuzzer, &mut executor, &mut mgr, &corpus_dirs)
            .unwrap_or_else(|err| {
                panic!(
                    "Failed to load initial corpus at {:?}: {:?}",
                    &corpus_dirs, err
                );
            });
        println!("imported {} inputs from disk.", state.corpus().count());
    }

    state.add_metadata(tokens);

    let mutator = StdScheduledMutator::new(havoc_mutations());

    let mut stages = tuple_list!(StdMutationalStage::new(mutator));
    // fuzzer
    //     .fuzz_loop_for(&mut stages, &mut executor, &mut state, &mut mgr, 10000)
    //     .unwrap();
    fuzzer
        .fuzz_loop(&mut stages, &mut executor, &mut state, &mut mgr)
        .expect("failed to start fuzzer");
    // mgr.on_restart(&mut state).unwrap();
}
