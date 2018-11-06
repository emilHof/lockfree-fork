use owned_alloc::OwnedAlloc;
use std::{
    ptr::{null_mut, NonNull},
    sync::atomic::{AtomicPtr, Ordering::*},
};

/// The error of `Sender::send` operation. Occurs if the receiver was
/// disconnected.
#[derive(Debug, Clone, Copy)]
pub struct NoRecv<T> {
    /// The message which was attempted to be sent.
    pub message: T,
}

/// The error of `Receiver::recv` operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvErr {
    /// Returned when there are no messages, the channel is empty, but the
    /// sender is still connected.
    NoMessage,
    /// Returned when the sender was disconnected.
    NoSender,
}

/// Creates an asynchronous lock-free Single-Producer-Single-Consumer (SPSC)
/// channel. The receiver does not provide any sort of `wait-for-message`
/// operation. It would not be lock-free otherwise. If you need such a
/// mechanism, consider using this channel with a `CondVar` (not lock-free).
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let alloc = OwnedAlloc::new(Node {
        val: None,
        next: AtomicPtr::new(null_mut()),
    });
    let nnptr = alloc.into_raw();

    (Sender { back: nnptr }, Receiver { front: nnptr })
}

/// The `Sender` handle of a SPSC channel. Created by `channel` function.
pub struct Sender<T> {
    back: NonNull<Node<T>>,
}

impl<T> Sender<T> {
    /// Sends a message and if the receiver disconnected, an error is returned.
    pub fn send(&mut self, val: T) -> Result<(), NoRecv<T>> {
        let alloc = OwnedAlloc::new(Node {
            val: Some(val),
            next: AtomicPtr::new(null_mut()),
        });
        let nnptr = alloc.into_raw();

        let res = unsafe { self.back.as_ref() }.next.compare_and_swap(
            null_mut(),
            nnptr.as_ptr(),
            Release,
        );

        if res.is_null() {
            self.back = nnptr;
            Ok(())
        } else {
            let (node, _) = unsafe { OwnedAlloc::from_raw(nnptr).move_inner() };
            Err(NoRecv {
                message: node.val.unwrap(),
            })
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let res = unsafe { self.back.as_ref() }.next.compare_and_swap(
            null_mut(),
            (null_mut::<Node<T>>() as usize | 1) as *mut _,
            Release,
        );

        if !res.is_null() {
            unsafe { OwnedAlloc::from_raw(self.back) };
        }
    }
}

unsafe impl<T> Send for Sender<T> where T: Send {}
unsafe impl<T> Sync for Sender<T> where T: Send {}

/// The `Receiver` handle of a SPSC channel. Created by `channel` function.
pub struct Receiver<T> {
    front: NonNull<Node<T>>,
}

impl<T> Receiver<T> {
    /// Tries to receive a message. If no message is available,
    /// `Err(RecvErr::NoMessage)` is returned. If the sender disconnected,
    /// `Err(RecvErr::NoSender)` is returned.
    pub fn recv(&mut self) -> Result<T, RecvErr> {
        loop {
            let node = unsafe { &mut *self.front.as_ptr() };

            match node.val.take() {
                Some(val) => {
                    let next = node.next.load(Acquire) as usize;

                    if let Some(nnptr) = NonNull::new((next & !1) as *mut _) {
                        unsafe { OwnedAlloc::from_raw(self.front) };
                        self.front = nnptr;
                    }

                    break Ok(val);
                },

                None => {
                    let next = node.next.load(Acquire);

                    if next as usize & 1 == 0 {
                        match NonNull::new(next) {
                            Some(nnptr) => {
                                unsafe { OwnedAlloc::from_raw(self.front) };
                                self.front = nnptr;
                            },

                            None => break Err(RecvErr::NoMessage),
                        }
                    } else {
                        break Err(RecvErr::NoSender);
                    }
                },
            }
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        loop {
            let next = unsafe { self.front.as_ref() }.next.compare_and_swap(
                null_mut(),
                (null_mut::<Node<T>>() as usize | 1) as *mut _,
                AcqRel,
            );

            let next_nnptr = match NonNull::new(next) {
                Some(nnptr) => nnptr,
                None => break,
            };

            unsafe { OwnedAlloc::from_raw(self.front) };

            if next as usize & 1 == 1 {
                break;
            }

            self.front = next_nnptr;
        }
    }
}

unsafe impl<T> Send for Receiver<T> where T: Send {}
unsafe impl<T> Sync for Receiver<T> where T: Send {}

#[repr(align(/* at least */ 2))]
struct Node<T> {
    val: Option<T>,
    next: AtomicPtr<Node<T>>,
}
#[cfg(test)]
mod test {
    use super::*;
    use std::thread;

    #[test]
    fn correct_sequence() {
        let (mut sender, mut receiver) = channel::<usize>();
        let thread = thread::spawn(move || {
            for i in 0 .. 512 {
                loop {
                    match receiver.recv() {
                        Ok(j) => {
                            assert_eq!(i, j);
                            break;
                        },

                        Err(RecvErr::NoMessage) => (),

                        _ => unreachable!(),
                    }
                }
            }
        });

        for i in 0 .. 512 {
            sender.send(i).unwrap();
        }

        thread.join().unwrap();
    }
}