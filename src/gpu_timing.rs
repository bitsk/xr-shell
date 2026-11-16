// SPDX-License-Identifier: GPL-3.0-or-later
//! Optional timestamp queries. Read during post-xrEndFrame retirement, after
//! the renderer's submission fence;
//! never add a profiling wait or interpret device ticks as CLOCK_MONOTONIC.
use crate::gpu::Gpu;
use ash::vk;
use serde::Serialize;
use std::rc::Rc;

#[derive(Clone, Default, Serialize)]
pub struct Sample {
    pub status: String,
    pub intervals_ns: Option<[f64; 4]>,
    pub timestamp_period_ns: f32,
    pub valid_bits: u32,
}
pub struct Timer {
    gpu: Rc<Gpu>,
    pool: vk::QueryPool,
    pub sample: Sample,
}
impl Timer {
    pub fn new(gpu: Rc<Gpu>) -> Self {
        let mut timer = Self {
            gpu,
            pool: vk::QueryPool::null(),
            sample: Sample {
                status: "disabled".into(),
                ..Default::default()
            },
        };
        if std::env::var("XR_SHELL_GPU_TIMING").as_deref() != Ok("1") {
            return timer;
        }
        unsafe {
            let families = timer
                .gpu
                .instance
                .get_physical_device_queue_family_properties(timer.gpu.physical);
            timer.sample.valid_bits = families[timer.gpu.family as usize].timestamp_valid_bits;
            timer.sample.timestamp_period_ns = timer
                .gpu
                .instance
                .get_physical_device_properties(timer.gpu.physical)
                .limits
                .timestamp_period;
            if timer.sample.valid_bits == 0 || timer.sample.timestamp_period_ns <= 0. {
                timer.sample.status = "unsupported".into();
                return timer;
            }
            match timer.gpu.device.create_query_pool(
                &vk::QueryPoolCreateInfo::default()
                    .query_type(vk::QueryType::TIMESTAMP)
                    .query_count(5),
                None,
            ) {
                Ok(pool) => {
                    timer.pool = pool;
                    timer.sample.status = "pending".into();
                }
                Err(error) => timer.sample.status = format!("query_pool: {error:?}"),
            }
        }
        timer
    }
    pub fn begin(&mut self, cmd: vk::CommandBuffer) {
        self.sample.intervals_ns = None;
        if self.pool == vk::QueryPool::null() {
            return;
        }
        unsafe {
            self.gpu.device.cmd_reset_query_pool(cmd, self.pool, 0, 5);
        }
        self.mark(cmd, 0);
    }
    pub fn mark(&self, cmd: vk::CommandBuffer, index: u32) {
        if self.pool == vk::QueryPool::null() {
            return;
        }
        unsafe {
            self.gpu.device.cmd_write_timestamp(
                cmd,
                if index == 0 {
                    vk::PipelineStageFlags::TOP_OF_PIPE
                } else {
                    vk::PipelineStageFlags::BOTTOM_OF_PIPE
                },
                self.pool,
                index,
            );
        }
    }
    pub fn resolve(&mut self) {
        if self.pool == vk::QueryPool::null() {
            return;
        }
        let mut ticks = [0u64; 5];
        match unsafe {
            self.gpu.device.get_query_pool_results(
                self.pool,
                0,
                &mut ticks,
                vk::QueryResultFlags::TYPE_64,
            )
        } {
            Ok(()) => {
                let mask = u64::MAX >> (64 - self.sample.valid_bits);
                self.sample.intervals_ns = Some(std::array::from_fn(|i| {
                    (ticks[i + 1].wrapping_sub(ticks[i]) & mask) as f64
                        * self.sample.timestamp_period_ns as f64
                }));
                self.sample.status = "available".into();
            }
            Err(error) => {
                self.sample.status = format!("query_result: {error:?}");
                self.sample.intervals_ns = None;
            }
        }
    }
}
impl Drop for Timer {
    fn drop(&mut self) {
        if self.pool != vk::QueryPool::null() {
            // Error paths may leave a submission in flight.
            unsafe {
                let _ = self.gpu.device.device_wait_idle();
                self.gpu.device.destroy_query_pool(self.pool, None);
            }
        }
    }
}
