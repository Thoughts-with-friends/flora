//! The renderer's pipelines: the on-disk cache, layouts and waiting for compilation.
use super::*;

impl LatteRenderer {
    /// Keeps the driver's compiled shaders in `dir` from now on, for the next run too.
    /// Without support in the backend this does nothing.
    pub fn use_pipeline_cache(&mut self, dir: &Path, adapter: &wgpu::AdapterInfo) {
        if !self.device.features().contains(wgpu::Features::PIPELINE_CACHE) {
            return;
        }
        let Some(name) = wgpu::util::pipeline_cache_key(adapter) else { return };
        let path = dir.join(name);
        // A damaged cache counts as no cache: not every driver survives one.
        let data = std::fs::read(&path).ok().and_then(|file| {
            let data = verified_cache_data(&file).map(<[u8]>::to_vec);
            if data.is_none() {
                log::warn!("{}: damaged shader cache thrown away", path.display());
                let _ = std::fs::remove_file(&path);
            }
            data
        });
        // SAFETY: the file is named after this adapter and driver, as wgpu asks, its checksum
        // holds, and `fallback` makes data the driver does not accept mean an empty cache.
        let cache = unsafe {
            self.device.create_pipeline_cache(&wgpu::PipelineCacheDescriptor {
                label: Some("shocora pipelines"),
                data: data.as_deref(),
                fallback: true,
            })
        };
        self.pipeline_cache = Some((cache, path));
    }

    pub(super) fn save_pipeline_cache(&self) {
        let Some((cache, path)) = &self.pipeline_cache else { return };
        let Some(mut data) = cache.get_data() else { return };
        data.extend_from_slice(&cache_checksum(&data).to_le_bytes());
        // Written aside and moved in place: a crash leaves the old file, not half of a new one.
        let partial = path.with_extension("partial");
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&partial, data))
            .and_then(|()| std::fs::rename(&partial, path));
        if let Err(error) = written {
            log::warn!("{}: {error}", path.display());
        }
    }

    /// Handle to choose whether draws wait for their pipelines while the renderer runs.
    #[inline]
    pub fn pipeline_setting(&self) -> PipelineSetting {
        self.pipeline_setting.clone()
    }

    /// Has the pipelines of `draws` compiled, as many at a time as there are cores to spare.
    pub(super) fn compile_pipelines(&mut self, draws: &[&DrawCommand]) {
        for draw in draws {
            let key = pipeline_key(draw);
            if self.pipelines.contains_key(&key) || !self.compiling.insert(key) {
                continue;
            }
            let layout = self.pipeline_layout(&sorted_bindings(draw), draw.vertex.stream_out);
            let job = PipelineJob {
                epoch: self.pipeline_epoch,
                key,
                draw: Box::new((*draw).clone()),
                layout,
                context: self.pipeline_context(),
            };
            if self.pipeline_jobs.send(job).is_err() {
                log::error!("the pipeline threads are gone");
                self.compiling.remove(&key);
                self.pipelines.insert(key, KnownPipeline { pipeline: None, last_used: self.flips });
            }
        }
    }

    fn pipeline_arrived(&mut self, (epoch, key, pipeline): FinishedPipeline) {
        if epoch == self.pipeline_epoch {
            self.compiling.remove(&key);
            self.pipelines.insert(key, KnownPipeline { pipeline, last_used: self.flips });
        }
    }

    /// Takes in compiled pipelines until none is left to come, or until `deadline` if any.
    pub(super) fn wait_for_pipelines(&mut self, deadline: Option<Instant>) {
        while !self.compiling.is_empty() {
            let arrived = match deadline {
                Some(deadline) => self
                    .finished_pipelines
                    .recv_timeout(deadline.saturating_duration_since(Instant::now())),
                None => self.finished_pipelines.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match arrived {
                Ok(finished) => self.pipeline_arrived(finished),
                Err(_) => return,
            }
        }
    }

    /// Waits for the pipeline of `key` alone.
    fn wait_for_pipeline(&mut self, key: PipelineKey) {
        while self.compiling.contains(&key) {
            let Ok(finished) = self.finished_pipelines.recv() else { return };
            self.pipeline_arrived(finished);
        }
    }

    pub(super) fn resource_layout(
        &mut self,
        bindings: &[(u32, BindingKind)],
    ) -> wgpu::BindGroupLayout {
        let device = &self.device;
        self.resource_layouts
            .entry(bindings.to_vec())
            .or_insert_with(|| {
                let entries: Vec<wgpu::BindGroupLayoutEntry> = bindings
                    .iter()
                    .map(|(binding, kind)| {
                        let visibility = if (*binding as usize) < 36 {
                            wgpu::ShaderStages::VERTEX
                        } else {
                            wgpu::ShaderStages::FRAGMENT
                        };
                        let ty = match kind {
                            BindingKind::Texture { dim, kind } => wgpu::BindingType::Texture {
                                sample_type: match kind {
                                    TextureKind::Float => {
                                        wgpu::TextureSampleType::Float { filterable: true }
                                    }
                                    TextureKind::Depth => wgpu::TextureSampleType::Depth,
                                    TextureKind::Uint => wgpu::TextureSampleType::Uint,
                                    TextureKind::Sint => wgpu::TextureSampleType::Sint,
                                },
                                view_dimension: view_dimension(*dim),
                                multisampled: false,
                            },
                            BindingKind::Sampler { comparison, filtering } => {
                                wgpu::BindingType::Sampler(if *comparison {
                                    wgpu::SamplerBindingType::Comparison
                                } else if *filtering {
                                    wgpu::SamplerBindingType::Filtering
                                } else {
                                    wgpu::SamplerBindingType::NonFiltering
                                })
                            }
                        };
                        wgpu::BindGroupLayoutEntry {
                            binding: *binding,
                            visibility,
                            ty,
                            count: None,
                        }
                    })
                    .collect();
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("shocora resources"),
                    entries: &entries,
                })
            })
            .clone()
    }

    fn pipeline_layout(
        &mut self,
        bindings: &[(u32, BindingKind)],
        stream_out: bool,
    ) -> wgpu::PipelineLayout {
        let resources = self.resource_layout(bindings);
        let device = &self.device;
        let arena_layout = &self.arena_layout;
        let stream_layout = self.stream.as_ref().filter(|_| stream_out).map(|s| &s.layout);
        self.pipeline_layouts
            .entry((bindings.to_vec(), stream_out))
            .or_insert_with(|| {
                device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("shocora pipeline"),
                    bind_group_layouts: &[Some(arena_layout), Some(&resources), stream_layout],
                    immediate_size: 0,
                })
            })
            .clone()
    }

    /// The draw's pipeline; `None` leaves the draw out. Stream-out feeds later draws and shows
    /// no picture that next frame's could replace, so it never goes without. It is compiled
    /// right here, as waiting in line behind a new scene's other pipelines could take seconds.
    pub(super) fn pipeline(&mut self, draw: &DrawCommand) -> Option<Arc<wgpu::RenderPipeline>> {
        let key = pipeline_key(draw);
        if !self.pipelines.contains_key(&key) {
            if draw.stream_only && !self.compiling.contains(&key) {
                let layout = self.pipeline_layout(&sorted_bindings(draw), draw.vertex.stream_out);
                let pipeline = self.pipeline_context().build(draw, &layout).map(Arc::new);
                self.pipelines.insert(key, KnownPipeline { pipeline, last_used: self.flips });
            } else {
                self.compile_pipelines(&[draw]);
                if draw.stream_only || !self.pipeline_setting.background() {
                    self.wait_for_pipeline(key);
                }
            }
        }
        let flips = self.flips;
        let known = self.pipelines.get_mut(&key)?;
        known.last_used = flips;
        known.pipeline.clone()
    }

    /// Lets go of what has not been needed for long, as Cemu does with its shader states.
    pub(super) fn clean_up_pipelines(&mut self) {
        let flips = self.flips;
        self.pipelines.retain(|_, known| {
            // A refusal costs nothing to remember, and would be logged again.
            known.pipeline.is_none()
                || flips.saturating_sub(known.last_used) < PIPELINE_EXPIRE_FLIPS
        });
        // Parsed shaders are large, and only needed to make pipelines: threads compiling right
        // now hold on to the map they got, and a shader needed again is parsed again.
        self.modules = Arc::default();
    }

    fn pipeline_context(&self) -> PipelineContext {
        PipelineContext {
            device: self.device.clone(),
            modules: Arc::clone(&self.modules),
            cache: self.pipeline_cache.as_ref().map(|(cache, _)| cache.clone()),
        }
    }
}

fn cache_checksum(data: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write(data);
    hasher.finish()
}

/// The contents of a pipeline cache file, if its checksum still matches them.
fn verified_cache_data(file: &[u8]) -> Option<&[u8]> {
    let (data, checksum) = file.split_at_checked(file.len().checked_sub(8)?)?;
    (cache_checksum(data).to_le_bytes().as_slice() == checksum).then_some(data)
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    #[test]
    fn a_damaged_cache_file_is_not_believed() {
        let mut file = vec![1, 2, 3, 4, 5];
        file.extend_from_slice(&cache_checksum(&file).to_le_bytes());
        assert_eq!(verified_cache_data(&file), Some([1, 2, 3, 4, 5].as_slice()));

        file[2] ^= 1;
        assert_eq!(verified_cache_data(&file), None);
        assert_eq!(verified_cache_data(&file[..file.len() - 3]), None);
        assert_eq!(verified_cache_data(&[1, 2]), None);
    }
}
