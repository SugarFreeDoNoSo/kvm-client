// Stable Rust 1.92 equivalents of newer standard-library conveniences.
use std::collections::VecDeque;
pub trait SliceArray<T> {
    fn compat_array<const N: usize>(&self) -> Option<&[T; N]>;
    fn compat_mut_array<const N: usize>(&mut self) -> Option<&mut [T; N]>;
}
impl<T> SliceArray<T> for [T] {
    fn compat_array<const N: usize>(&self) -> Option<&[T; N]> { self.try_into().ok() }
    fn compat_mut_array<const N: usize>(&mut self) -> Option<&mut [T; N]> { self.try_into().ok() }
}
pub trait DequePopIf<T> { fn pop_front_matching(&mut self, predicate: impl FnOnce(&mut T) -> bool) -> Option<T>; }
impl<T> DequePopIf<T> for VecDeque<T> {
    fn pop_front_matching(&mut self, predicate: impl FnOnce(&mut T) -> bool) -> Option<T> {
        if self.front_mut().is_some_and(predicate) { self.pop_front() } else { None }
    }
}
