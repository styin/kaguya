//! Action executor — maps `PipelineAction` values to real side effects.
//!
//! The executor is intentionally thin: it holds references to all Gateway
//! components and performs one I/O call per action variant. All decision
//! logic lives in the handlers; the executor is a mechanical dispatch layer.

use std::sync::Arc;

use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info};

use crate::context;
use crate::history::History;
use crate::output::OutputManager;
use crate::persona::Persona;
use crate::proto;
use crate::rag::RagEngine;
use crate::reasoner::ReasonerManager;
use crate::silence::SilenceTimers;
use crate::talker::TalkerClient;
use crate::tools::ToolRegistry;
use crate::types::InputEvent;

use super::types::{PipelineAction, TurnState};

/// Bridges [`PipelineAction`] values to real component calls.
/// Constructed per select-branch in the event loop.
pub struct ActionExecutor<'a> {
    pub talker: &'a TalkerClient,
    pub history: &'a History,
    pub output: &'a OutputManager,
    pub tools: &'a ToolRegistry,
    pub reasoner: &'a ReasonerManager,
    pub rag: &'a Arc<RagEngine>,
    pub silence: &'a SilenceTimers,
    pub persona: &'a Persona,
    pub shared_persona: &'a Arc<RwLock<proto::PersonaConfig>>,
    pub talker_output_tx: mpsc::Sender<proto::TalkerOutput>,
    pub p3_tx: mpsc::Sender<InputEvent>,
    pub state: &'a mut TurnState,
}

impl<'a> ActionExecutor<'a> {
    /// Execute actions sequentially in the order returned by the handler.
    pub async fn execute_all(&mut self, actions: Vec<PipelineAction>) {
        for action in actions {
            self.execute_one(action).await;
        }
    }

    async fn execute_one(&mut self, action: PipelineAction) {
        match action {
            // ── Talker dispatch ──
            PipelineAction::DispatchTalker { context, kind } => {
                debug!(?kind, "executor: dispatching Talker");
                let token = self
                    .talker
                    .dispatch(context, self.talker_output_tx.clone())
                    .await;
                self.state.active_gen = Some(token);
            }

            PipelineAction::BargeIn => {
                self.talker.barge_in(&self.state.conversation_id).await;
            }

            // ── Output routing ──
            PipelineAction::SendResponseStarted { turn_id } => {
                self.output.send_response_started(&turn_id).await;
            }

            PipelineAction::SendSentence { text } => {
                self.output.send_sentence(&text).await;
            }

            PipelineAction::SendEmotion { emotion } => {
                self.output.send_emotion(&emotion).await;
            }

            PipelineAction::SendResponseComplete {
                turn_id,
                was_interrupted,
            } => {
                self.output
                    .send_response_complete(&turn_id, was_interrupted)
                    .await;
            }

            PipelineAction::SendUserInput { text } => {
                self.output.send_user_input(&text).await;
            }

            PipelineAction::MuteOutput => {
                self.output.mute_audio();
            }

            PipelineAction::UnmuteOutput => {
                self.output.unmute_audio();
            }

            // ── History ──
            PipelineAction::AppendUserHistory { text } => {
                self.history.append_user(&text).await;
            }

            PipelineAction::AppendAssistantHistory { text } => {
                self.history.append_assistant(&text).await;
            }

            PipelineAction::AppendAssistantPartialHistory { spoken_text } => {
                self.history.append_assistant_partial(&spoken_text).await;
            }

            PipelineAction::AppendToolResultHistory { tool_name, content } => {
                self.history.append_tool_result(&tool_name, &content).await;
            }

            // ── Tool / Reasoner dispatch ──
            PipelineAction::DispatchTool {
                request_id,
                tool_name,
                args_json,
            } => {
                self.tools
                    .dispatch(request_id, tool_name, args_json, self.p3_tx.clone());
            }

            PipelineAction::StartReasoner {
                task_id,
                description,
            } => {
                self.reasoner
                    .start(task_id, description, self.p3_tx.clone())
                    .await;
            }

            // ── Post-turn ──
            PipelineAction::EvaluateAndStoreMemory {
                user_input,
                assistant_response,
                turn_id,
            } => {
                self.rag
                    .evaluate_and_store(&user_input, &assistant_response, &turn_id)
                    .await;
            }

            PipelineAction::UpdatePersonaIfChanged => {
                let new_memory_md = self.rag.export_memory_md().await;
                if new_memory_md != self.state.last_memory_md {
                    info!("memory changed, pushing updated persona to Talker");
                    self.state.last_memory_md = new_memory_md;
                    let new_persona = proto::PersonaConfig {
                        soul_md: self.persona.soul().await,
                        identity_md: self.persona.identity().await,
                        memory_md: self.state.last_memory_md.clone(),
                    };
                    *self.shared_persona.write().await = new_persona.clone();
                    self.talker.update_persona(new_persona).await;
                }
            }

            PipelineAction::PrefillCache => {
                let tasks = self.reasoner.active_tasks().await;
                let pctx = context::for_prefill(
                    &self.state.conversation_id,
                    self.history,
                    &self.state.last_memory_md,
                    self.tools,
                    &tasks,
                )
                .await;
                self.talker
                    .prefill_cache(&self.state.conversation_id, pctx)
                    .await;
            }

            // ── Silence ──
            PipelineAction::StartSilenceTimers => {
                if let Some(t) = self.state.active_silence.take() {
                    t.cancel();
                }
                self.state.active_silence = Some(self.silence.start());
            }
        }
    }
}
