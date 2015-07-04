/*!
An implementation of a multi-producer, multi-consumer synchronous channel with
a (possible empty) fixed size buffer.
*/

extern crate rand;
extern crate uuid;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::ops;
use std::sync::{Arc, Condvar, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};

use rand::Rng;
use uuid::Uuid;

pub trait Channel {
    type Item;

    fn send(&self, val: Self::Item);
    fn try_send(&self, val: Self::Item) -> Result<(), Self::Item>;
    fn recv(&self) -> Option<Self::Item>;
    fn try_recv(&self) -> Result<Option<Self::Item>, ()>;
    fn close(&self);
    fn subscribe(&self, condvar: Arc<Condvar>) -> Uuid;
    fn unsubscribe(&self, key: &Uuid);
    fn clone_chan(&self) -> Self where Self: Sized;

    fn iter(self) -> Iter<Self> where Self: Sized { Iter::new(self) }
}

impl<'a, T: Channel> Channel for &'a T {
    type Item = T::Item;

    fn send(&self, val: Self::Item) { (*self).send(val); }
    fn try_send(&self, val: Self::Item) -> Result<(), Self::Item> {
        (*self).try_send(val)
    }
    fn recv(&self) -> Option<Self::Item> { (*self).recv() }
    fn try_recv(&self) -> Result<Option<Self::Item>, ()> { (*self).try_recv() }
    fn close(&self) { (*self).close() }
    fn subscribe(&self, condvar: Arc<Condvar>) -> Uuid {
        (*self).subscribe(condvar)
    }
    fn unsubscribe(&self, key: &Uuid) { (*self).unsubscribe(key) }
    fn clone_chan(&self) -> Self where Self: Sized {
        self
    }
}

#[derive(Debug)]
pub struct SyncChannel<T>(Arc<SyncInner<T>>);

struct Notifier(Mutex<RefCell<HashMap<Uuid, Arc<Condvar>>>>);

#[derive(Debug)]
enum SyncInner<T> {
    Unbuffered(Unbuffered<T>),
    Buffered(Buffered<T>),
}

struct Unbuffered<T> {
    notify: Notifier,
    nwaiting: AtomicUsize,
    cond: Condvar,
    sender: Mutex<()>,
    val: Mutex<RefCell<UnbufferedValue<T>>>,
}

#[derive(Debug)]
struct UnbufferedValue<T> {
    val: Option<T>,
    closed: bool,
}

struct Buffered<T> {
    notify: Notifier,
    cap: usize,
    cond: Condvar,
    ring: Mutex<RefCell<Ring<T>>>,
}

#[derive(Debug)]
struct Ring<T> {
    queue: Vec<Option<T>>,
    pos: usize,
    len: usize,
    closed: bool,
}

impl<T> SyncChannel<T> {
    pub fn new(size: usize) -> SyncChannel<T> {
        let inner = if size == 0 {
            SyncInner::Unbuffered(Unbuffered {
                notify: Notifier::new(),
                nwaiting: AtomicUsize::new(0),
                cond: Condvar::new(),
                sender: Mutex::new(()),
                val: Mutex::new(RefCell::new(UnbufferedValue {
                    val: None,
                    closed: false,
                })),
            })
        } else {
            let mut queue = Vec::with_capacity(size);
            for _ in 0..size { queue.push(None); }
            SyncInner::Buffered(Buffered {
                notify: Notifier::new(),
                cap: size,
                cond: Condvar::new(),
                ring: Mutex::new(RefCell::new(Ring {
                    queue: queue,
                    pos: 0,
                    len: 0,
                    closed: false,
                })),
            })
        };
        SyncChannel(Arc::new(inner))
    }
}

impl Notifier {
    fn new() -> Notifier {
        Notifier(Mutex::new(RefCell::new(HashMap::new())))
    }

    fn notify(&self) {
        let notify = self.0.lock().unwrap();
        for condvar in notify.borrow().values() {
            condvar.notify_all();
        }
    }

    fn subscribe(&self, condvar: Arc<Condvar>) -> Uuid {
        let notify = self.0.lock().unwrap();
        let mut notify = notify.borrow_mut();
        for _ in 0..10 {
            let key = Uuid::new_v4();
            if !notify.contains_key(&key) {
                notify.insert(key.clone(), condvar);
                return key;
            }
        }
        panic!("could not generate channel subscription key")
    }

    fn unsubscribe(&self, key: &Uuid) {
        let notify = self.0.lock().unwrap();
        notify.borrow_mut().remove(key);
    }
}

impl<T> Clone for SyncChannel<T> {
    fn clone(&self) -> SyncChannel<T> {
        SyncChannel(self.0.clone())
    }
}

impl<T> Channel for SyncChannel<T> {
    type Item = T;

    fn send(&self, val: T) {
        match *self.0 {
            SyncInner::Unbuffered(ref i) => i.send(val, false).ok().unwrap(),
            SyncInner::Buffered(ref i) => i.send(val, false).ok().unwrap(),
        }
    }

    fn try_send(&self, val: T) -> Result<(), T> {
        match *self.0 {
            SyncInner::Unbuffered(ref i) => i.send(val, true),
            SyncInner::Buffered(ref i) => i.send(val, true),
        }
    }

    fn recv(&self) -> Option<T> {
        match *self.0 {
            SyncInner::Unbuffered(ref i) => i.recv(false).unwrap(),
            SyncInner::Buffered(ref i) => i.recv(false).unwrap(),
        }
    }

    fn try_recv(&self) -> Result<Option<T>, ()> {
        match *self.0 {
            SyncInner::Unbuffered(ref i) => i.recv(true),
            SyncInner::Buffered(ref i) => i.recv(true),
        }
    }

    fn close(&self) {
        match *self.0 {
            SyncInner::Unbuffered(ref i) => i.close(),
            SyncInner::Buffered(ref i) => i.close(),
        }
    }

    fn subscribe(&self, condvar: Arc<Condvar>) -> Uuid {
        match *self.0 {
            SyncInner::Unbuffered(ref i) => i.notify.subscribe(condvar),
            SyncInner::Buffered(ref i) => i.notify.subscribe(condvar),
        }
    }

    fn unsubscribe(&self, key: &Uuid) {
        match *self.0 {
            SyncInner::Unbuffered(ref i) => i.notify.unsubscribe(key),
            SyncInner::Buffered(ref i) => i.notify.unsubscribe(key),
        }
    }

    fn clone_chan(&self) -> SyncChannel<T> {
        self.clone()
    }
}

impl<T> Buffered<T> {
    fn send(&self, val: T, try: bool) -> Result<(), T> {
        let mut ring = self.ring.lock().unwrap();
        // We *need* two of these checks. This is here because if the
        // channel is already closed, then the condition variable may
        // never be woken up again, and thus, we'll be dead-locked.
        if ring.borrow().closed {
            panic!("cannot send on a closed channel");
        }
        while ring.borrow().len == self.cap {
            if try {
                return Err(val);
            }
            ring = self.cond.wait(ring).unwrap();
        }
        // ... and this is necessary because the channel may have been
        // closed while we were waiting for the queue to empty. And we
        // absolutely cannot abide adding to the queue if the channel
        // has been closed.
        if ring.borrow().closed {
            panic!("cannot send on a closed channel");
        }
        ring.borrow_mut().push(val);
        self.cond.notify_all();
        self.notify.notify();
        Ok(())
    }

    fn recv(&self, try: bool) -> Result<Option<T>, ()> {
        let mut ring = self.ring.lock().unwrap();
        while ring.borrow().len == 0 {
            if ring.borrow().closed {
                return Ok(None);
            }
            if try {
                return Err(());
            }
            ring = self.cond.wait(ring).unwrap();
        }
        let val = ring.borrow_mut().pop();
        self.cond.notify_all();
        self.notify.notify();
        Ok(Some(val))
    }

    fn close(&self) {
        let ring = self.ring.lock().unwrap();
        ring.borrow_mut().closed = true;
        self.cond.notify_all();
        self.notify.notify();
    }
}

impl<T> Ring<T> {
    fn push(&mut self, val: T) {
        let (pos, len, cap) = (self.pos, self.len, self.queue.len());
        assert!(len < cap);
        self.queue[(pos + len) % cap] = Some(val);
        self.len += 1;
    }

    fn pop(&mut self) -> T {
        let (pos, len, cap) = (self.pos, self.len, self.queue.len());
        assert!(len <= cap);
        assert!(len > 0);
        let val = self.queue[pos].take().expect("non-null item in queue");
        self.pos = (pos + 1) % cap;
        self.len -= 1;
        val
    }
}

impl<T> Unbuffered<T> {
    fn send(&self, send_val: T, try: bool) -> Result<(), T> {
        {
            let _sender_lock = self.sender.lock().unwrap();
            if try && self.nwaiting.load(Ordering::SeqCst) == 0 {
                return Err(send_val);
            }
            // Since the sender lock has been acquired, that implies any
            // previous senders have completed, which implies that all
            // receivers that could make progress have made progress, and the
            // rest are blocked. Therefore, `val` must be `None`.
            let mut val = self.val.lock().unwrap();
            if val.borrow().closed {
                panic!("cannot send on a closed channel");
            }
            val.borrow_mut().val = Some(send_val);
            self.cond.notify_all();
            self.notify.notify();
            // At this point, any blocked receivers have woken up and will race
            // to access `val`. So we release the mutex but continue blocking
            // until a receiver has retrieved the value.
            // If there are no blocked receivers, then we continue blocking
            // until there is one that grabs the value.
            while val.borrow().val.is_some() {
                // It's possible we could wake up here by the broadcast from
                // `close`, but that's OK: the value was added to the queue
                // before `close` was called, which means a receiver can still
                // retrieve it.
                val = self.cond.wait(val).unwrap();
            }
            // OK, if we're here, then the value we put in has been slurped up
            // by a received *and* we've re-acquired the `val` lock. Now we
            // release it and the sender lock to permit other senders to try.
        }
        // We notify after the lock has been released so that the next time
        // a sender tries to send, it will absolutely not be blocked by *this*
        // send.
        self.notify.notify();
        Ok(())
    }

    fn recv(&self, try: bool) -> Result<Option<T>, ()> {
        let mut val = self.val.lock().unwrap();
        while val.borrow().val.is_none() {
            if val.borrow().closed {
                return Ok(None);
            }
            if try {
                return Err(());
            }
            self.nwaiting.fetch_add(1, Ordering::SeqCst);
            val = self.cond.wait(val).unwrap();
            self.nwaiting.fetch_sub(1, Ordering::SeqCst);
        }
        let recv_val = val.borrow_mut().val.take().unwrap();
        self.cond.notify_all();
        self.notify.notify();
        Ok(Some(recv_val))
    }

    fn close(&self) {
        let val = self.val.lock().unwrap();
        val.borrow_mut().closed = true;
        // If there are any blocked receivers, this will wake them up and
        // force them to return.
        self.cond.notify_all();
        self.notify.notify();
    }
}


pub struct Iter<C> {
    chan: C,
}

impl<C: Channel> Iter<C> {
    pub fn new(c: C) -> Iter<C> { Iter { chan: c } }
}

impl<C: Channel> Iterator for Iter<C> {
    type Item = C::Item;
    fn next(&mut self) -> Option<C::Item> { self.chan.recv() }
}

impl fmt::Debug for Notifier {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let notify = self.0.lock().unwrap();
        let notify = notify.borrow();
        writeln!(f, "Notifier({:?})", notify.keys().collect::<Vec<_>>())
    }
}

impl<T: fmt::Debug> fmt::Debug for Unbuffered<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let val = self.val.lock().unwrap();
        let val = val.borrow();
        try!(writeln!(f, "Unbuffered {{"));
        try!(writeln!(f, "    notify: {:?}", self.notify));
        try!(writeln!(f, "    nwaiting: {:?}",
                      self.nwaiting.load(Ordering::SeqCst)));
        try!(writeln!(f, "    val: {:?}", val));
        try!(writeln!(f, "}}"));
        Ok(())
    }
}

impl<T: fmt::Debug> fmt::Debug for Buffered<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let ring = self.ring.lock().unwrap();
        let ring = ring.borrow();
        try!(writeln!(f, "Buffered {{"));
        try!(writeln!(f, "    notify: {:?}", self.notify));
        try!(writeln!(f, "    cap: {:?}", self.cap));
        try!(writeln!(f, "    ring: {:?}", ring));
        try!(writeln!(f, "}}"));
        Ok(())
    }
}

#[derive(Debug)]
pub struct AsyncChannel<T>(Arc<AsyncInner<T>>);

struct AsyncInner<T> {
    notify: Notifier,
    cond: Condvar,
    queue: Mutex<RefCell<AsyncQueue<T>>>,
}

struct AsyncQueue<T> {
    queue: VecDeque<T>,
    closed: bool,
}

impl<T> AsyncChannel<T> {
    pub fn new() -> AsyncChannel<T> {
        AsyncChannel(Arc::new(AsyncInner {
            notify: Notifier::new(),
            cond: Condvar::new(),
            queue: Mutex::new(RefCell::new(AsyncQueue {
                queue: VecDeque::with_capacity(1024),
                closed: false,
            })),
        }))
    }

    fn _recv(&self, try: bool) -> Result<Option<T>, ()> {
        let mut queue = self.0.queue.lock().unwrap();
        while queue.borrow().queue.len() == 0 {
            if queue.borrow().closed {
                return Ok(None);
            }
            if try {
                return Err(());
            }
            queue = self.0.cond.wait(queue).unwrap();
        }
        let val = queue.borrow_mut().queue.pop_front().unwrap();
        self.0.cond.notify_all();
        self.0.notify.notify();
        Ok(Some(val))
    }
}

impl<T> Channel for AsyncChannel<T> {
    type Item = T;

    fn send(&self, val: T) {
        self.try_send(val).ok().unwrap();
    }

    fn try_send(&self, val: T) -> Result<(), T> {
        let queue = self.0.queue.lock().unwrap();
        queue.borrow_mut().queue.push_back(val);
        self.0.cond.notify_all();
        self.0.notify.notify();
        Ok(())
    }

    fn recv(&self) -> Option<T> {
        self._recv(false).unwrap()
    }

    fn try_recv(&self) -> Result<Option<T>, ()> {
        self._recv(true)
    }

    fn close(&self) {
        let queue = self.0.queue.lock().unwrap();
        queue.borrow_mut().closed = true;
        self.0.cond.notify_all();
        self.0.notify.notify();
    }

    fn subscribe(&self, condvar: Arc<Condvar>) -> Uuid {
        self.0.notify.subscribe(condvar)
    }

    fn unsubscribe(&self, key: &Uuid) {
        self.0.notify.unsubscribe(key)
    }

    fn clone_chan(&self) -> AsyncChannel<T> {
        self.clone()
    }
}

impl<T> Clone for AsyncChannel<T> {
    fn clone(&self) -> AsyncChannel<T> {
        AsyncChannel(self.0.clone())
    }
}

impl<T: fmt::Debug> fmt::Debug for AsyncInner<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let queue = self.queue.lock().unwrap();
        let queue = queue.borrow();
        try!(writeln!(f, "AsyncInner {{"));
        try!(writeln!(f, "    notify: {:?}", self.notify));
        try!(writeln!(f, "    closed: {:?}", queue.closed));
        try!(writeln!(f, "    queue: {:?}", queue.queue));
        try!(writeln!(f, "}}"));
        Ok(())
    }
}

#[derive(Clone)]
pub struct WaitGroup(Arc<WaitGroupInner>);

struct WaitGroupInner {
    cond: Condvar,
    count: Mutex<Cell<i32>>,
}

impl WaitGroup {
    pub fn new() -> WaitGroup {
        WaitGroup(Arc::new(WaitGroupInner {
            cond: Condvar::new(),
            count: Mutex::new(Cell::new(0)),
        }))
    }

    pub fn add(&self, delta: i32) {
        let count = self.0.count.lock().unwrap();
        count.set(count.get() + delta);
        assert!(count.get() >= 0);
        self.0.cond.notify_all();
    }

    pub fn done(&self) {
        self.add(-1);
    }

    pub fn wait(&self) {
        let mut count = self.0.count.lock().unwrap();
        while count.get() > 0 {
            count = self.0.cond.wait(count).unwrap();
        }
    }
}

impl fmt::Debug for WaitGroup {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let count = self.0.count.lock().unwrap();
        write!(f, "WaitGroup {{ count: {:?} }}", count.get())
    }
}

pub struct Choose<'a, T> {
    cond: Arc<Condvar>,
    cond_mutex: Mutex<()>,
    chans: Vec<(Uuid, Box<Channel<Item=T> + 'a>)>,
}

impl<'a, T> Choose<'a, T> {
    pub fn new() -> Choose<'a, T> {
        Choose {
            cond: Arc::new(Condvar::new()),
            cond_mutex: Mutex::new(()),
            chans: vec![],
        }
    }

    pub fn recv<C>(mut self, chan: C) -> Choose<'a, T>
            where C: Channel<Item=T> + 'a {
        let key = chan.subscribe(self.cond.clone());
        self.chans.push((key, Box::new(chan)));
        self
    }

    pub fn choose(&mut self) -> Option<T> {
        loop {
            match self.try_choose() {
                Ok(v) => return v,
                Err(()) => {}
            }
            let _ = self.cond.wait(self.cond_mutex.lock().unwrap()).unwrap();
        }
    }

    pub fn try_choose(&mut self) -> Result<Option<T>, ()> {
        let mut rng = rand::thread_rng();
        rng.shuffle(&mut self.chans);
        for &(_, ref chan) in &self.chans {
            match chan.try_recv() {
                Ok(v) => return Ok(v),
                Err(()) => continue,
            }
        }
        Err(())
    }
}

impl<'a, T> ops::Drop for Choose<'a, T> {
    fn drop(&mut self) {
        for &(ref key, ref chan) in &self.chans {
            chan.unsubscribe(key);
        }
    }
}

pub struct Select<'a> {
    cond: Arc<Condvar>,
    cond_mutex: Mutex<()>,
    choices: Vec<Choice<'a>>,
    default: Option<Box<FnMut() + 'a>>,
}

struct Choice<'a> {
    run: Box<FnMut() -> bool + 'a>,
    unsubscribe: Box<Fn() + 'a>,
}

impl<'a> Select<'a> {
    pub fn new() -> Select<'a> {
        Select {
            cond: Arc::new(Condvar::new()),
            cond_mutex: Mutex::new(()),
            choices: vec![],
            default: None,
        }
    }

    pub fn select(mut self) {
        let mut first = true;
        while !self.try() {
            if first && self.default.is_some() {
                self.default.take().unwrap()();
                return;
            }
            first = false;
            let _ = self.cond.wait(self.cond_mutex.lock().unwrap()).unwrap();
        }
    }

    fn try(&mut self) -> bool {
        let mut rng = rand::thread_rng();
        rng.shuffle(&mut self.choices);
        for choice in &mut self.choices {
            if (choice.run)() {
                return true;
            }
        }
        false
    }

    pub fn default<F>(mut self, run: F) -> Select<'a> where F: FnMut() + 'a {
        self.default = Some(Box::new(run));
        self
    }

    pub fn send<'c: 'a, 'b: 'a, C, T, F>(
        mut self,
        chan: C,
        val: T,
        mut run: F,
    ) -> Select<'a> where C: Channel<Item=T> + 'c,
                          T: 'b,
                          F: FnMut() + 'a {
        let key = chan.subscribe(self.cond.clone());
        let mut val = Some(val);
        let chan2 = chan.clone_chan();
        self.choices.push(Choice {
            run: Box::new(move || {
                match chan.try_send(val.take().unwrap()) {
                    Ok(()) => { run(); true }
                    Err(val2) => { val = Some(val2); false }
                }
            }),
            unsubscribe: Box::new(move || chan2.unsubscribe(&key)),
        });
        self
    }

    pub fn recv<'b: 'a, C, T, F>(
        mut self,
        chan: C,
        mut run: F,
    ) -> Select<'a> where C: Channel<Item=T> + 'b,
                          F: FnMut(Option<T>) + 'a {
        let key = chan.subscribe(self.cond.clone());
        let chan2 = chan.clone_chan();
        self.choices.push(Choice {
            run: Box::new(move || {
                match chan.try_recv() {
                    Ok(val) => { run(val); true }
                    Err(()) => false,
                }
            }),
            unsubscribe: Box::new(move || chan2.unsubscribe(&key)),
        });
        self
    }
}

impl<'a> ops::Drop for Select<'a> {
    fn drop(&mut self) {
        for choice in &mut self.choices {
            (choice.unsubscribe)();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::{Channel, Choose, Select, AsyncChannel, SyncChannel, WaitGroup};

    #[test]
    fn simple() {
        let chan = SyncChannel::new(1);
        chan.send(5);
        assert_eq!(chan.recv(), Some(5));
    }

    #[test]
    #[should_panic]
    fn no_send_on_close() {
        let chan = SyncChannel::new(1);
        chan.close();
        chan.send(5);
    }

    #[test]
    #[should_panic]
    fn no_send_on_close_unbuffered() {
        let chan = SyncChannel::new(0);
        chan.close();
        chan.send(5);
    }

    #[test]
    fn simple_unbuffered() {
        let chan = SyncChannel::new(0);
        { let chan = chan.clone(); thread::spawn(move || chan.send(5)); }
        assert_eq!(chan.recv(), Some(5));
    }

    #[test]
    fn simple_async() {
        let chan = AsyncChannel::new();
        chan.send(5);
        assert_eq!(chan.recv(), Some(5));
    }

    #[test]
    fn simple_iter() {
        let chan = SyncChannel::new(1);
        let chan2 = chan.clone();
        thread::spawn(move || {
            for i in 0..100 {
                chan2.send(i);
            }
            chan2.close();
        });
        let recvd: Vec<i32> = chan.iter().collect();
        assert_eq!(recvd, (0..100).collect::<Vec<i32>>());
    }

    #[test]
    fn simple_iter_unbuffered() {
        let chan = SyncChannel::new(0);
        let chan2 = chan.clone();
        thread::spawn(move || {
            for i in 0..100 {
                chan2.send(i);
            }
            chan2.close();
        });
        let recvd: Vec<i32> = chan.iter().collect();
        assert_eq!(recvd, (0..100).collect::<Vec<i32>>());
    }

    #[test]
    fn simple_iter_async() {
        let chan = AsyncChannel::new();
        let chan2 = chan.clone();
        thread::spawn(move || {
            for i in 0..100 {
                chan2.send(i);
            }
            chan2.close();
        });
        let recvd: Vec<i32> = chan.iter().collect();
        assert_eq!(recvd, (0..100).collect::<Vec<i32>>());
    }

    #[test]
    fn simple_try() {
        let chan = SyncChannel::new(1);
        chan.try_send(5).is_err();
        chan.try_recv().is_err();
    }

    #[test]
    fn simple_try_unbuffered() {
        let chan = SyncChannel::new(0);
        chan.try_send(5).is_err();
        chan.try_recv().is_err();
    }

    #[test]
    fn simple_try_async() {
        let chan = AsyncChannel::new();
        chan.try_recv().is_err();
        chan.try_send(5).is_ok();
    }

    #[test]
    fn select() {
        let ticka = SyncChannel::new(1);
        let tickb = SyncChannel::new(1);
        let tickc = SyncChannel::new(1);
        let recv = SyncChannel::new(0);
        {
            let ticka = ticka.clone();
            let tickb = tickb.clone();
            let tickc = tickc.clone();
            let recv = recv.clone();
            thread::spawn(move || {
                loop {
                    ticka.send("ticka");
                    thread::sleep_ms(100);
                    recv.recv();
                }
            });
            thread::spawn(move || {
                loop {
                    tickb.send("tickb");
                    thread::sleep_ms(50);
                }
            });
            thread::spawn(move || {
                thread::sleep_ms(1000);
                tickc.send(());
            });
        }
        loop {
            let mut stop = false;
            Select::new()
            .recv(&ticka, |val| println!("{:?}", val))
            .recv(&tickb, |val| println!("{:?}", val))
            .recv(&tickc, |val| { stop = true; println!("{:?}", val) })
            .send(&recv, (), || println!("SENT!"))
            .select();
            if stop {
                break;
            }
        }
        println!("done!");
    }

    #[test]
    fn choose() {
        #[derive(Debug)]
        enum Message { Foo, Bar, Fubar }
        let s1 = SyncChannel::new(1);
        let s2 = SyncChannel::new(1);
        let s3 = SyncChannel::new(1);
        {
            let (s1, s2, s3) = (s1.clone(), s2.clone(), s3.clone());
            thread::spawn(move || loop {
                thread::sleep_ms(50);
                s1.send(Message::Foo);
            });
            thread::spawn(move || loop {
                thread::sleep_ms(70);
                s2.send(Message::Bar);
            });
            thread::spawn(move || loop {
                thread::sleep_ms(500);
                s3.send(Message::Fubar);
            });
        }
        let mut chooser = Choose::new().recv(s1).recv(s2).recv(s3);
        loop {
            match chooser.choose().unwrap() {
                Message::Foo => println!("foo"),
                Message::Bar => println!("bar"),
                Message::Fubar => break,
            }
        }
        println!("done!");
    }

    #[test]
    fn mpmc() {
        let chan = SyncChannel::new(1);
        let mut done = vec![];
        for i in 0..4 {
            let chan = chan.clone();
            let cdone = SyncChannel::new(0);
            done.push(cdone.clone());
            thread::spawn(move || {
                for work in vec!['a', 'b', 'c'] {
                    chan.send((i, work));
                }
                cdone.send(());
            });
        }
        {
            // Wait for all of the producers to finish before closing
            // the channel.
            //
            // See below for similar example with wait groups that makes this
            // simpler.
            let chan = chan.clone();
            thread::spawn(move || {
                for cdone in done {
                    cdone.recv();
                }
                chan.close();
            });
        }
        for i in 0..4 {
            let chan = chan.clone();
            thread::spawn(move || {
                for (sent_from, work) in chan.iter() {
                    println!("sent from {} to {}, work: {}",
                             sent_from, i, work);
                }
                println!("worker {} done!", i);
            });
        }
        thread::sleep_ms(1000);
    }

    #[test]
    fn mpmc_wait_group() {
        let chan = SyncChannel::new(1);
        let wg = WaitGroup::new();
        for i in 0..4 {
            wg.add(1);
            let wg = wg.clone();
            let chan = chan.clone();
            thread::spawn(move || {
                for work in vec!['a', 'b', 'c'] {
                    chan.send((i, work));
                }
                wg.done();
            });
        }
        let wg_done = WaitGroup::new();
        for i in 0..4 {
            wg_done.add(1);
            let wg_done = wg_done.clone();
            let chan = chan.clone();
            thread::spawn(move || {
                for (sent_from, work) in chan.iter() {
                    println!("sent from {} to {}, work: {}",
                             sent_from, i, work);
                }
                println!("worker {} done!", i);
                wg_done.done();
            });
        }
        wg.wait();
        chan.close();
        wg_done.wait();
        println!("done!");
    }
}